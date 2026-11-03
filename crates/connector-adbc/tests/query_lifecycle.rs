use std::{
    collections::{HashSet, VecDeque},
    pin::Pin,
    sync::{
        Arc, Barrier, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

use adbc_core::{
    Connection, Database, Driver, Optionable, PartitionedResult, Statement,
    error::{Error, Result, Status},
    options::{
        AdbcVersion, InfoCode, ObjectDepth, OptionConnection, OptionDatabase, OptionStatement,
        OptionValue,
    },
};
use adbc_driver_manager::ManagedDriver;
use adbc_ffi::FFI_AdbcDriverInitFunc;
use arrow_array::{ArrayRef, Int64Array, RecordBatch, RecordBatchReader};
use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
use connector_adbc::{AdbcQueryStream, query};
use connector_sdk::{ConnectorError, ErrorKind};
use futures_core::Stream;

#[derive(Clone, Debug, PartialEq)]
enum Event {
    NewStatement,
    SetSql(String),
    Bind(RecordBatch),
    Execute,
    Pull,
    Cancel,
    BlockExit(BlockAt),
    ReaderDrop,
    StatementDrop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum BlockAt {
    NewStatement,
    SetSql,
    Execute,
    Pull,
}

const TEST_TIMEOUT: Duration = Duration::from_secs(2);

type QueryPull = Poll<Option<std::result::Result<RecordBatch, ConnectorError>>>;
type PullWorker = thread::JoinHandle<(QueryPull, AdbcQueryStream)>;

#[test]
fn public_query_covers_the_managed_connection_lifecycle() {
    happy_path_is_lazy_and_ordered();
    absent_parameters_are_not_bound();
    execute_failure_is_not_retried();
    abandonment_releases_the_reader_first();
    stream_error_releases_the_reader_first();
    cancellation_before_first_read_avoids_source_io();
    cancellation_during_statement_creation_waits_for_creation_to_return();
    cancellation_interrupts_startup();
    cancellation_interrupts_execute_without_waiting_for_completion();
    cancellation_interrupts_result_pull();
    failed_native_cancellation_preserves_the_query();
    cancellation_is_scoped_to_one_execution();
    cancellation_after_completion_is_a_noop_for_the_next_query();
    cancellation_between_reads_releases_resources_without_native_cancellation();
    cancellation_and_drop_are_linearized();
}

fn happy_path_is_lazy_and_ordered() {
    let fixture = Fixture::batches(vec![batch(&[10, 20]), batch(&[30])]);
    let connection = managed_connection();
    let parameters = batch(&[7]);
    let mut stream = query(
        connection,
        "select value where value = ?",
        Some(parameters.clone()),
    );

    assert_eq!(fixture.events(), Vec::<Event>::new());
    assert_eq!(pull(&mut stream), Poll::Ready(Some(Ok(batch(&[10, 20])))));
    assert_eq!(
        fixture.events(),
        vec![
            Event::NewStatement,
            Event::SetSql("select value where value = ?".to_owned()),
            Event::Bind(parameters),
            Event::Execute,
            Event::Pull,
        ]
    );

    assert_eq!(pull(&mut stream), Poll::Ready(Some(Ok(batch(&[30])))));
    assert_eq!(pull(&mut stream), Poll::Ready(None));
    assert_eq!(pull(&mut stream), Poll::Ready(None));
    assert_eq!(fixture.count(Event::Execute), 1);
    assert_release_order(&fixture);
}

fn absent_parameters_are_not_bound() {
    let fixture = Fixture::batches(vec![]);
    let mut stream = query(managed_connection(), "select 1", None);

    assert_eq!(pull(&mut stream), Poll::Ready(None));
    assert_eq!(fixture.count(Event::Execute), 1);
    assert_eq!(
        fixture.count_matching(|event| matches!(event, Event::Bind(_))),
        0
    );
}

fn execute_failure_is_not_retried() {
    let fixture = Fixture::failing_execute();
    let mut stream = query(managed_connection(), "select broken", None);

    let Poll::Ready(Some(Err(error))) = pull(&mut stream) else {
        panic!("first pull should return the fixture execution error");
    };
    assert_eq!(error.kind(), ErrorKind::Internal);
    assert_eq!(error.data_source_message(), Some("fixture execute failure"));
    assert_eq!(pull(&mut stream), Poll::Ready(None));
    assert_eq!(fixture.count(Event::NewStatement), 1);
    assert_eq!(fixture.count(Event::Execute), 1);
    assert_eq!(fixture.count(Event::StatementDrop), 1);
}

fn abandonment_releases_the_reader_first() {
    let fixture = Fixture::batches(vec![batch(&[1]), batch(&[2])]);
    let mut stream = query(managed_connection(), "select value", None);

    assert!(matches!(pull(&mut stream), Poll::Ready(Some(Ok(_)))));
    drop(stream);

    assert_release_order(&fixture);
}

fn stream_error_releases_the_reader_first() {
    let fixture = Fixture::stream_error();
    let mut stream = query(managed_connection(), "select broken stream", None);

    let Poll::Ready(Some(Err(error))) = pull(&mut stream) else {
        panic!("first pull should return the fixture stream error");
    };
    assert_eq!(error.kind(), ErrorKind::DataSource);
    assert_eq!(
        error.safe_message(),
        "the data source result stream failed without structured diagnostics"
    );
    assert_eq!(error.data_source_code(), None);
    assert!(error.diagnostics().is_empty());
    assert!(
        error
            .data_source_message()
            .is_some_and(|message| message.contains("fixture stream failure"))
    );
    assert_eq!(pull(&mut stream), Poll::Ready(None));

    assert_release_order(&fixture);
}

fn cancellation_before_first_read_avoids_source_io() {
    let fixture = Fixture::batches(vec![batch(&[1])]);
    let mut stream = query(managed_connection(), "select value", None);
    let cancel = stream.cancel_handle();

    cancel.try_cancel().unwrap();

    assert_cancelled(pull(&mut stream));
    assert_eq!(pull(&mut stream), Poll::Ready(None));
    assert_eq!(fixture.count(Event::NewStatement), 0);
    assert_eq!(fixture.count(Event::Execute), 0);
}

fn cancellation_during_statement_creation_waits_for_creation_to_return() {
    let fixture = Fixture::blocking(BlockAt::NewStatement, vec![batch(&[1])]);
    let stream = query(managed_connection(), "select value", None);
    let cancel = stream.cancel_handle();
    let worker = pull_on_worker(stream);

    fixture.wait_until_blocked(0);
    cancel.try_cancel().unwrap();
    assert_eq!(fixture.count(Event::Cancel), 0);

    fixture.release_block(0);
    let (result, _stream) = worker.join().unwrap();

    assert_cancelled(result);
    assert_eq!(fixture.count(Event::Execute), 0);
    assert_eq!(fixture.count(Event::ReaderDrop), 0);
    assert_eq!(fixture.count(Event::StatementDrop), 1);
}

fn cancellation_interrupts_startup() {
    let fixture = Fixture::blocking(BlockAt::SetSql, vec![batch(&[1])]);
    let stream = query(managed_connection(), "select value", None);
    let cancel = stream.cancel_handle();
    let worker = pull_on_worker(stream);

    fixture.wait_until_blocked(0);
    cancel.try_cancel().unwrap();
    let (result, _stream) = worker.join().unwrap();

    assert_cancelled(result);
    fixture.assert_cancel_preceded_block_exit(BlockAt::SetSql);
    assert_eq!(fixture.count(Event::Cancel), 1);
    assert_eq!(fixture.count(Event::Execute), 0);
    assert_eq!(fixture.count(Event::ReaderDrop), 0);
    assert_eq!(fixture.count(Event::StatementDrop), 1);
}

fn cancellation_interrupts_execute_without_waiting_for_completion() {
    let fixture = Fixture::blocking(BlockAt::Execute, vec![batch(&[1])]);
    let stream = query(managed_connection(), "select value", None);
    let cancel = stream.cancel_handle();
    let worker = pull_on_worker(stream);

    fixture.wait_until_blocked(0);
    cancel.try_cancel().unwrap();
    let (result, _stream) = worker.join().unwrap();

    assert_cancelled(result);
    fixture.assert_cancel_preceded_block_exit(BlockAt::Execute);
    assert_eq!(fixture.count(Event::Cancel), 1);
    assert_eq!(fixture.count(Event::Execute), 1);
    assert_eq!(fixture.count(Event::ReaderDrop), 0);
    assert_eq!(fixture.count(Event::StatementDrop), 1);
}

fn cancellation_interrupts_result_pull() {
    let fixture = Fixture::blocking(BlockAt::Pull, vec![batch(&[1])]);
    let stream = query(managed_connection(), "select value", None);
    let cancel = stream.cancel_handle();
    let worker = pull_on_worker(stream);

    fixture.wait_until_blocked(0);
    cancel.try_cancel().unwrap();
    let (result, _stream) = worker.join().unwrap();

    assert_cancelled(result);
    fixture.assert_cancel_preceded_block_exit(BlockAt::Pull);
    assert_eq!(fixture.count(Event::Cancel), 1);
    assert_release_order(&fixture);
}

fn failed_native_cancellation_preserves_the_query() {
    let fixture = Fixture::blocking_with_failing_cancel(BlockAt::Pull, vec![batch(&[1])]);
    let stream = query(managed_connection(), "select value", None);
    let cancel = stream.cancel_handle();
    let worker = pull_on_worker(stream);

    fixture.wait_until_blocked(0);

    let first_error = cancel.try_cancel().unwrap_err();
    assert_eq!(first_error.status, Status::NotImplemented);
    let second_error = cancel.try_cancel().unwrap_err();
    assert_eq!(second_error.status, Status::NotImplemented);

    fixture.release_block(0);
    let (result, mut stream) = worker.join().unwrap();
    assert_eq!(result, Poll::Ready(Some(Ok(batch(&[1])))));
    assert_eq!(pull(&mut stream), Poll::Ready(None));
    assert_eq!(fixture.count(Event::Cancel), 2);
}

fn cancellation_is_scoped_to_one_execution() {
    let fixture = Fixture::blocking(BlockAt::Pull, vec![batch(&[1])]);
    let connection = managed_connection();
    let first = query(connection.clone(), "select first", None);
    let first_cancel = first.cancel_handle();
    let first_worker = pull_on_worker(first);

    fixture.wait_until_blocked(0);

    let mut second = query(connection, "select second", None);
    assert_eq!(pull(&mut second), Poll::Ready(Some(Ok(batch(&[1])))));

    first_cancel.try_cancel().unwrap();
    let (first_result, _first) = first_worker.join().unwrap();
    assert_cancelled(first_result);

    assert_eq!(pull(&mut second), Poll::Ready(None));
    assert_eq!(fixture.count(Event::Cancel), 1);
    assert_eq!(fixture.count(Event::Execute), 2);
}

fn cancellation_after_completion_is_a_noop_for_the_next_query() {
    let fixture = Fixture::batches(vec![]);
    let connection = managed_connection();
    let mut first = query(connection.clone(), "select first", None);
    let late_cancel = first.cancel_handle();

    assert_eq!(pull(&mut first), Poll::Ready(None));
    late_cancel.try_cancel().unwrap();

    let mut second = query(connection, "select second", None);
    assert_eq!(pull(&mut second), Poll::Ready(None));
    assert_eq!(fixture.count(Event::Cancel), 0);
    assert_eq!(fixture.count(Event::Execute), 2);
}

fn cancellation_between_reads_releases_resources_without_native_cancellation() {
    let fixture = Fixture::batches(vec![batch(&[1]), batch(&[2])]);
    let mut stream = query(managed_connection(), "select value", None);
    let cancel = stream.cancel_handle();

    assert!(matches!(pull(&mut stream), Poll::Ready(Some(Ok(_)))));
    cancel.try_cancel().unwrap();

    assert_eq!(fixture.count(Event::Cancel), 0);
    assert_release_order(&fixture);

    assert_cancelled(pull(&mut stream));
    assert_eq!(fixture.count(Event::ReaderDrop), 1);
    assert_eq!(fixture.count(Event::StatementDrop), 1);
}

fn cancellation_and_drop_are_linearized() {
    for _ in 0..100 {
        let fixture = Fixture::batches(vec![batch(&[1]), batch(&[2])]);
        let mut stream = query(managed_connection(), "select value", None);
        let cancel = stream.cancel_handle();
        assert!(matches!(pull(&mut stream), Poll::Ready(Some(Ok(_)))));

        let barrier = Arc::new(Barrier::new(3));
        let cancel_barrier = Arc::clone(&barrier);
        let cancel_worker = thread::spawn(move || {
            cancel_barrier.wait();
            cancel.try_cancel()
        });
        let drop_barrier = Arc::clone(&barrier);
        let drop_worker = thread::spawn(move || {
            drop_barrier.wait();
            drop(stream);
        });

        barrier.wait();
        cancel_worker.join().unwrap().unwrap();
        drop_worker.join().unwrap();

        assert_eq!(fixture.count(Event::Cancel), 0);
        assert_release_order(&fixture);
    }
}

fn pull_on_worker(mut stream: AdbcQueryStream) -> PullWorker {
    thread::spawn(move || {
        let result = pull(&mut stream);
        (result, stream)
    })
}

fn assert_cancelled(result: QueryPull) {
    let Poll::Ready(Some(Err(error))) = result else {
        panic!("query should end with cancellation");
    };
    assert_eq!(error.kind(), ErrorKind::Cancellation);
}

fn wait_for(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !condition() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(condition(), "fixture did not reach the expected state");
}

fn managed_connection() -> adbc_driver_manager::ManagedConnection {
    let mut driver = ManagedDriver::load_static(
        &(AdbcConnectorFixtureInit as FFI_AdbcDriverInitFunc),
        AdbcVersion::V110,
    )
    .unwrap();
    let database = driver.new_database().unwrap();
    database.new_connection().unwrap()
}

fn assert_release_order(fixture: &Fixture) {
    let events = fixture.events();
    let reader_drop = events
        .iter()
        .position(|event| *event == Event::ReaderDrop)
        .unwrap();
    let statement_drop = events
        .iter()
        .position(|event| *event == Event::StatementDrop)
        .unwrap();
    assert!(reader_drop < statement_drop);
    assert_eq!(fixture.count(Event::ReaderDrop), 1);
    assert_eq!(fixture.count(Event::StatementDrop), 1);
}

fn pull<S>(stream: &mut S) -> Poll<Option<S::Item>>
where
    S: Stream + Unpin,
{
    let mut context = Context::from_waker(Waker::noop());
    Stream::poll_next(Pin::new(stream), &mut context)
}

fn batch(values: &[i64]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "value",
        DataType::Int64,
        false,
    )]));
    let columns: Vec<ArrayRef> = vec![Arc::new(Int64Array::from(values.to_vec()))];
    RecordBatch::try_new(schema, columns).unwrap()
}

#[derive(Clone)]
struct Fixture {
    events: Arc<Mutex<Vec<Event>>>,
    controls: Arc<Mutex<Vec<Arc<StatementControl>>>>,
}

impl Fixture {
    fn batches(batches: Vec<RecordBatch>) -> Self {
        Self::install(
            batches.into_iter().map(FixtureItem::Batch).collect(),
            false,
            None,
            None,
        )
    }

    fn failing_execute() -> Self {
        Self::install(vec![], true, None, None)
    }

    fn stream_error() -> Self {
        Self::install(vec![FixtureItem::Error], false, None, None)
    }

    fn blocking(block_at: BlockAt, batches: Vec<RecordBatch>) -> Self {
        Self::install(
            batches.into_iter().map(FixtureItem::Batch).collect(),
            false,
            Some((0, block_at)),
            None,
        )
    }

    fn blocking_with_failing_cancel(block_at: BlockAt, batches: Vec<RecordBatch>) -> Self {
        Self::install(
            batches.into_iter().map(FixtureItem::Batch).collect(),
            false,
            Some((0, block_at)),
            Some(0),
        )
    }

    fn install(
        items: Vec<FixtureItem>,
        fail_execute: bool,
        block_at: Option<(usize, BlockAt)>,
        fail_cancel_statement: Option<usize>,
    ) -> Self {
        let fixture = Self {
            events: Arc::new(Mutex::new(Vec::new())),
            controls: Arc::new(Mutex::new(Vec::new())),
        };
        *FIXTURE.lock().unwrap() = Some(FixturePlan {
            events: Arc::clone(&fixture.events),
            controls: Arc::clone(&fixture.controls),
            items,
            fail_execute,
            next_statement: Arc::new(AtomicUsize::new(0)),
            block_at,
            fail_cancel_statement,
        });
        fixture
    }

    fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    fn count(&self, needle: Event) -> usize {
        self.count_matching(|event| *event == needle)
    }

    fn count_matching(&self, predicate: impl Fn(&Event) -> bool) -> usize {
        self.events()
            .iter()
            .filter(|event| predicate(event))
            .count()
    }

    fn assert_cancel_preceded_block_exit(&self, block_at: BlockAt) {
        let events = self.events();
        let cancel = events
            .iter()
            .position(|event| *event == Event::Cancel)
            .unwrap();
        let block_exit = events
            .iter()
            .position(|event| *event == Event::BlockExit(block_at))
            .unwrap();
        assert!(
            cancel < block_exit,
            "cancel must overlap the blocked callback"
        );
    }

    fn control(&self, index: usize) -> Arc<StatementControl> {
        wait_for(|| self.controls.lock().unwrap().len() > index);
        Arc::clone(&self.controls.lock().unwrap()[index])
    }

    fn wait_until_blocked(&self, index: usize) {
        let control = self.control(index);
        wait_for(|| control.block_entered.load(SeqCst));
    }

    fn release_block(&self, index: usize) {
        self.control(index).release_block.store(true, SeqCst);
    }
}

#[derive(Clone)]
struct FixturePlan {
    events: Arc<Mutex<Vec<Event>>>,
    controls: Arc<Mutex<Vec<Arc<StatementControl>>>>,
    items: Vec<FixtureItem>,
    fail_execute: bool,
    next_statement: Arc<AtomicUsize>,
    block_at: Option<(usize, BlockAt)>,
    fail_cancel_statement: Option<usize>,
}

struct StatementControl {
    events: Arc<Mutex<Vec<Event>>>,
    block_at: Option<BlockAt>,
    block_entered: AtomicBool,
    release_block: AtomicBool,
    fail_cancel: bool,
    cancelled: AtomicBool,
}

impl StatementControl {
    fn block(&self, point: BlockAt) -> Result<()> {
        if self.block_at != Some(point) {
            return Ok(());
        }

        self.block_entered.store(true, SeqCst);
        let deadline = Instant::now() + TEST_TIMEOUT;
        while !self.cancelled.load(SeqCst)
            && !self.release_block.load(SeqCst)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(5));
        }
        if self.cancelled.load(SeqCst) {
            self.events.lock().unwrap().push(Event::BlockExit(point));
            Err(Error::with_message_and_status(
                "fixture query was cancelled",
                Status::Cancelled,
            ))
        } else if self.release_block.load(SeqCst) {
            self.events.lock().unwrap().push(Event::BlockExit(point));
            Ok(())
        } else {
            self.events.lock().unwrap().push(Event::BlockExit(point));
            Err(Error::with_message_and_status(
                "fixture timed out waiting for cancellation",
                Status::Internal,
            ))
        }
    }

    fn cancel(&self) -> Result<()> {
        self.events.lock().unwrap().push(Event::Cancel);
        if self.fail_cancel {
            return Err(Error::with_message_and_status(
                "fixture cancellation is not implemented",
                Status::NotImplemented,
            ));
        }
        self.cancelled.store(true, SeqCst);
        Ok(())
    }
}

#[derive(Clone)]
struct FixtureCancelHandle {
    control: Arc<StatementControl>,
}

impl adbc_core::CancelHandle for FixtureCancelHandle {
    fn try_cancel(&self) -> Result<()> {
        self.control.cancel()
    }
}

#[derive(Clone)]
enum FixtureItem {
    Batch(RecordBatch),
    Error,
}

static FIXTURE: LazyLock<Mutex<Option<FixturePlan>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Default)]
struct FixtureDriver;

impl Driver for FixtureDriver {
    type DatabaseType = FixtureDatabase;

    fn new_database(&mut self) -> Result<Self::DatabaseType> {
        let plan = FIXTURE.lock().unwrap().take().ok_or_else(|| {
            Error::with_message_and_status("fixture was not installed", Status::InvalidState)
        })?;
        Ok(FixtureDatabase { plan })
    }

    fn new_database_with_opts(
        &mut self,
        _opts: impl IntoIterator<Item = (OptionDatabase, OptionValue)>,
    ) -> Result<Self::DatabaseType> {
        self.new_database()
    }
}

struct FixtureDatabase {
    plan: FixturePlan,
}

impl Optionable for FixtureDatabase {
    type Option = OptionDatabase;

    fn set_option(&mut self, _key: Self::Option, _value: OptionValue) -> Result<()> {
        unsupported()
    }

    fn get_option_string(&self, _key: Self::Option) -> Result<String> {
        unsupported()
    }

    fn get_option_bytes(&self, _key: Self::Option) -> Result<Vec<u8>> {
        unsupported()
    }

    fn get_option_int(&self, _key: Self::Option) -> Result<i64> {
        unsupported()
    }

    fn get_option_double(&self, _key: Self::Option) -> Result<f64> {
        unsupported()
    }
}

impl Database for FixtureDatabase {
    type ConnectionType = FixtureConnection;

    fn new_connection(&self) -> Result<Self::ConnectionType> {
        Ok(FixtureConnection {
            plan: self.plan.clone(),
        })
    }

    fn new_connection_with_opts(
        &self,
        _opts: impl IntoIterator<Item = (OptionConnection, OptionValue)>,
    ) -> Result<Self::ConnectionType> {
        self.new_connection()
    }
}

struct FixtureConnection {
    plan: FixturePlan,
}

impl Optionable for FixtureConnection {
    type Option = OptionConnection;

    fn set_option(&mut self, _key: Self::Option, _value: OptionValue) -> Result<()> {
        unsupported()
    }

    fn get_option_string(&self, _key: Self::Option) -> Result<String> {
        unsupported()
    }

    fn get_option_bytes(&self, _key: Self::Option) -> Result<Vec<u8>> {
        unsupported()
    }

    fn get_option_int(&self, _key: Self::Option) -> Result<i64> {
        unsupported()
    }

    fn get_option_double(&self, _key: Self::Option) -> Result<f64> {
        unsupported()
    }
}

impl Connection for FixtureConnection {
    type StatementType = FixtureStatement;

    fn new_statement(&mut self) -> Result<Self::StatementType> {
        self.plan.events.lock().unwrap().push(Event::NewStatement);
        let statement_index = self.plan.next_statement.fetch_add(1, SeqCst);
        let control = Arc::new(StatementControl {
            events: Arc::clone(&self.plan.events),
            block_at: self
                .plan
                .block_at
                .and_then(|(index, block_at)| (index == statement_index).then_some(block_at)),
            block_entered: AtomicBool::new(false),
            release_block: AtomicBool::new(false),
            fail_cancel: self.plan.fail_cancel_statement == Some(statement_index),
            cancelled: AtomicBool::new(false),
        });
        self.plan
            .controls
            .lock()
            .unwrap()
            .push(Arc::clone(&control));
        control.block(BlockAt::NewStatement)?;
        Ok(FixtureStatement {
            plan: self.plan.clone(),
            control,
        })
    }

    fn get_info(
        &self,
        _codes: Option<HashSet<InfoCode>>,
    ) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        unsupported()
    }

    fn get_objects(
        &self,
        _depth: ObjectDepth,
        _catalog: Option<&str>,
        _db_schema: Option<&str>,
        _table_name: Option<&str>,
        _table_type: Option<Vec<&str>>,
        _column_name: Option<&str>,
    ) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        unsupported()
    }

    fn get_table_schema(
        &self,
        _catalog: Option<&str>,
        _db_schema: Option<&str>,
        _table_name: &str,
    ) -> Result<Schema> {
        unsupported()
    }

    fn get_table_types(&self) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        unsupported()
    }

    fn get_statistic_names(&self) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        unsupported()
    }

    fn get_statistics(
        &self,
        _catalog: Option<&str>,
        _db_schema: Option<&str>,
        _table_name: Option<&str>,
        _approximate: bool,
    ) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        unsupported()
    }

    fn commit(&mut self) -> Result<()> {
        unsupported()
    }

    fn rollback(&mut self) -> Result<()> {
        unsupported()
    }

    fn read_partition(
        &self,
        _partition: impl AsRef<[u8]>,
    ) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        unsupported()
    }
}

struct FixtureStatement {
    plan: FixturePlan,
    control: Arc<StatementControl>,
}

impl Drop for FixtureStatement {
    fn drop(&mut self) {
        self.plan.events.lock().unwrap().push(Event::StatementDrop);
    }
}

impl Optionable for FixtureStatement {
    type Option = OptionStatement;

    fn set_option(&mut self, _key: Self::Option, _value: OptionValue) -> Result<()> {
        unsupported()
    }

    fn get_option_string(&self, _key: Self::Option) -> Result<String> {
        unsupported()
    }

    fn get_option_bytes(&self, _key: Self::Option) -> Result<Vec<u8>> {
        unsupported()
    }

    fn get_option_int(&self, _key: Self::Option) -> Result<i64> {
        unsupported()
    }

    fn get_option_double(&self, _key: Self::Option) -> Result<f64> {
        unsupported()
    }
}

impl Statement for FixtureStatement {
    fn get_cancel_handle(&self) -> Box<dyn adbc_core::CancelHandle> {
        Box::new(FixtureCancelHandle {
            control: Arc::clone(&self.control),
        })
    }

    fn bind(&mut self, batch: RecordBatch) -> Result<()> {
        self.plan.events.lock().unwrap().push(Event::Bind(batch));
        Ok(())
    }

    fn bind_stream(&mut self, _reader: Box<dyn RecordBatchReader + Send>) -> Result<()> {
        unsupported()
    }

    fn execute(&mut self) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        self.plan.events.lock().unwrap().push(Event::Execute);
        self.control.block(BlockAt::Execute)?;
        if self.plan.fail_execute {
            return Err(Error::with_message_and_status(
                "fixture execute failure",
                Status::Internal,
            ));
        }
        Ok(Box::new(FixtureReader {
            events: Arc::clone(&self.plan.events),
            control: Arc::clone(&self.control),
            schema: self
                .plan
                .items
                .first()
                .and_then(|item| match item {
                    FixtureItem::Batch(batch) => Some(batch.schema()),
                    FixtureItem::Error => None,
                })
                .unwrap_or_else(|| Arc::new(Schema::empty())),
            items: self.plan.items.clone().into(),
        }))
    }

    fn execute_update(&mut self) -> Result<Option<i64>> {
        unsupported()
    }

    fn execute_schema(&mut self) -> Result<Schema> {
        unsupported()
    }

    fn execute_partitions(&mut self) -> Result<PartitionedResult> {
        unsupported()
    }

    fn get_parameter_schema(&self) -> Result<Schema> {
        unsupported()
    }

    fn prepare(&mut self) -> Result<()> {
        unsupported()
    }

    fn set_sql_query(&mut self, query: impl AsRef<str>) -> Result<()> {
        self.plan
            .events
            .lock()
            .unwrap()
            .push(Event::SetSql(query.as_ref().to_owned()));
        self.control.block(BlockAt::SetSql)
    }

    fn set_substrait_plan(&mut self, _plan: impl AsRef<[u8]>) -> Result<()> {
        unsupported()
    }
}

struct FixtureReader {
    events: Arc<Mutex<Vec<Event>>>,
    control: Arc<StatementControl>,
    schema: SchemaRef,
    items: VecDeque<FixtureItem>,
}

impl Iterator for FixtureReader {
    type Item = std::result::Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.events.lock().unwrap().push(Event::Pull);
        if let Err(error) = self.control.block(BlockAt::Pull) {
            return Some(Err(ArrowError::ExternalError(Box::new(error))));
        }
        self.items.pop_front().map(|item| match item {
            FixtureItem::Batch(batch) => Ok(batch),
            FixtureItem::Error => Err(ArrowError::ExternalError(Box::new(
                Error::with_message_and_status("fixture stream failure", Status::Internal),
            ))),
        })
    }
}

impl RecordBatchReader for FixtureReader {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
}

impl Drop for FixtureReader {
    fn drop(&mut self) {
        self.events.lock().unwrap().push(Event::ReaderDrop);
    }
}

fn unsupported<T>() -> Result<T> {
    Err(Error::with_message_and_status(
        "fixture operation is not implemented",
        Status::NotImplemented,
    ))
}

adbc_ffi::export_driver!(AdbcConnectorFixtureInit, FixtureDriver);
