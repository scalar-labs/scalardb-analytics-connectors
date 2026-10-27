use std::{
    collections::{HashSet, VecDeque},
    pin::Pin,
    sync::{Arc, LazyLock, Mutex},
    task::{Context, Poll, Waker},
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
use connector_adbc::query;
use futures_core::Stream;

#[derive(Clone, Debug, PartialEq)]
enum Event {
    NewStatement,
    SetSql(String),
    Bind(RecordBatch),
    Execute,
    Pull,
    ReaderDrop,
    StatementDrop,
}

#[test]
fn public_query_covers_the_managed_connection_lifecycle() {
    happy_path_is_lazy_and_ordered();
    absent_parameters_are_not_bound();
    execute_failure_is_not_retried();
    abandonment_releases_the_reader_first();
    stream_error_releases_the_reader_first();
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
    assert_eq!(error.message, "fixture execute failure");
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
    assert!(error.message.contains("fixture stream failure"));
    assert_eq!(pull(&mut stream), Poll::Ready(None));

    assert_release_order(&fixture);
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
}

impl Fixture {
    fn batches(batches: Vec<RecordBatch>) -> Self {
        Self::install(batches.into_iter().map(FixtureItem::Batch).collect(), false)
    }

    fn failing_execute() -> Self {
        Self::install(vec![], true)
    }

    fn stream_error() -> Self {
        Self::install(vec![FixtureItem::Error], false)
    }

    fn install(items: Vec<FixtureItem>, fail_execute: bool) -> Self {
        let fixture = Self {
            events: Arc::new(Mutex::new(Vec::new())),
        };
        *FIXTURE.lock().unwrap() = Some(FixturePlan {
            events: Arc::clone(&fixture.events),
            items,
            fail_execute,
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
}

#[derive(Clone)]
struct FixturePlan {
    events: Arc<Mutex<Vec<Event>>>,
    items: Vec<FixtureItem>,
    fail_execute: bool,
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
        Ok(FixtureStatement {
            plan: self.plan.clone(),
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
    fn bind(&mut self, batch: RecordBatch) -> Result<()> {
        self.plan.events.lock().unwrap().push(Event::Bind(batch));
        Ok(())
    }

    fn bind_stream(&mut self, _reader: Box<dyn RecordBatchReader + Send>) -> Result<()> {
        unsupported()
    }

    fn execute(&mut self) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        self.plan.events.lock().unwrap().push(Event::Execute);
        if self.plan.fail_execute {
            return Err(Error::with_message_and_status(
                "fixture execute failure",
                Status::Internal,
            ));
        }
        Ok(Box::new(FixtureReader {
            events: Arc::clone(&self.plan.events),
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
        Ok(())
    }

    fn set_substrait_plan(&mut self, _plan: impl AsRef<[u8]>) -> Result<()> {
        unsupported()
    }
}

struct FixtureReader {
    events: Arc<Mutex<Vec<Event>>>,
    schema: SchemaRef,
    items: VecDeque<FixtureItem>,
}

impl Iterator for FixtureReader {
    type Item = std::result::Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.events.lock().unwrap().push(Event::Pull);
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
