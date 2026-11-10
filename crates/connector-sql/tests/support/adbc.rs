use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Condvar, LazyLock, Mutex},
};

use adbc_core::{
    CancelHandle, Connection, Database, Driver, Optionable, PartitionedResult, Statement,
    error::{Error, Result, Status},
    options::{
        AdbcVersion, InfoCode, ObjectDepth, OptionConnection, OptionDatabase, OptionStatement,
        OptionValue,
    },
};
use adbc_driver_manager::{ManagedConnection, ManagedDriver};
use adbc_ffi::FFI_AdbcDriverInitFunc;
use arrow_array::{RecordBatch, RecordBatchReader};
use arrow_schema::{ArrowError, Schema, SchemaRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    NewStatement,
    SetSql(String),
    Execute,
    Pull,
    Cancel,
}

#[derive(Clone)]
pub(crate) struct Fixture {
    events: Arc<Mutex<Vec<Event>>>,
    gate: Arc<ExecuteGate>,
}

#[allow(dead_code)]
pub(crate) enum FixtureExecution {
    Batches(Vec<RecordBatch>),
    Fail,
    /// `execute` blocks at the source until the statement is cancelled.
    BlockUntilCancelled,
}

impl Fixture {
    pub(crate) fn install(execution: FixtureExecution) -> (Self, ManagedConnection) {
        let fixture = Self {
            events: Arc::new(Mutex::new(Vec::new())),
            gate: Arc::new(ExecuteGate::default()),
        };
        let (batches, execute) = match execution {
            FixtureExecution::Batches(batches) => (batches, ExecuteBehavior::Succeed),
            FixtureExecution::Fail => (Vec::new(), ExecuteBehavior::Fail),
            FixtureExecution::BlockUntilCancelled => {
                (Vec::new(), ExecuteBehavior::BlockUntilCancelled)
            }
        };
        *FIXTURE.lock().unwrap() = Some(FixturePlan {
            events: Arc::clone(&fixture.events),
            gate: Arc::clone(&fixture.gate),
            batches,
            execute,
        });
        (fixture, managed_connection())
    }

    pub(crate) fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }

    /// Waits until a blocking `execute` has started at the source.
    #[allow(dead_code)]
    pub(crate) fn wait_until_executing(&self) {
        let state = self.gate.state.lock().unwrap();
        drop(
            self.gate
                .changed
                .wait_while(state, |state| !state.executing)
                .unwrap(),
        );
    }
}

#[derive(Clone)]
struct FixturePlan {
    events: Arc<Mutex<Vec<Event>>>,
    gate: Arc<ExecuteGate>,
    batches: Vec<RecordBatch>,
    execute: ExecuteBehavior,
}

#[derive(Clone, Copy)]
enum ExecuteBehavior {
    Succeed,
    Fail,
    BlockUntilCancelled,
}

/// Coordinates a blocking `execute` with the test thread and statement cancellation.
#[derive(Default)]
struct ExecuteGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    executing: bool,
    cancelled: bool,
}

static FIXTURE: LazyLock<Mutex<Option<FixturePlan>>> = LazyLock::new(|| Mutex::new(None));

fn managed_connection() -> ManagedConnection {
    let mut driver = ManagedDriver::load_static(
        &(SqlExecutableQueryFixtureInit as FFI_AdbcDriverInitFunc),
        AdbcVersion::V110,
    )
    .unwrap();
    let database = driver.new_database().unwrap();
    database.new_connection().unwrap()
}

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
    fn get_cancel_handle(&self) -> Box<dyn CancelHandle> {
        Box::new(FixtureCancelHandle {
            events: Arc::clone(&self.plan.events),
            gate: Arc::clone(&self.plan.gate),
        })
    }

    fn bind(&mut self, _batch: RecordBatch) -> Result<()> {
        unsupported()
    }

    fn bind_stream(&mut self, _reader: Box<dyn RecordBatchReader + Send>) -> Result<()> {
        unsupported()
    }

    fn execute(&mut self) -> Result<Box<dyn RecordBatchReader + Send + 'static>> {
        self.plan.events.lock().unwrap().push(Event::Execute);
        match self.plan.execute {
            ExecuteBehavior::Succeed => {}
            ExecuteBehavior::Fail => {
                return Err(Error::with_message_and_status(
                    "fixture execute failure",
                    Status::Internal,
                ));
            }
            ExecuteBehavior::BlockUntilCancelled => {
                let mut state = self.plan.gate.state.lock().unwrap();
                state.executing = true;
                self.plan.gate.changed.notify_all();
                drop(
                    self.plan
                        .gate
                        .changed
                        .wait_while(state, |state| !state.cancelled)
                        .unwrap(),
                );
                return Err(Error::with_message_and_status(
                    "fixture execute cancelled",
                    Status::Cancelled,
                ));
            }
        }
        let schema = self
            .plan
            .batches
            .first()
            .map(RecordBatch::schema)
            .unwrap_or_else(|| Arc::new(Schema::empty()));
        Ok(Box::new(FixtureReader {
            events: Arc::clone(&self.plan.events),
            schema,
            batches: self.plan.batches.clone().into(),
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
    batches: VecDeque<RecordBatch>,
}

impl Iterator for FixtureReader {
    type Item = std::result::Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.events.lock().unwrap().push(Event::Pull);
        self.batches.pop_front().map(Ok)
    }
}

impl RecordBatchReader for FixtureReader {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
}

/// Records statement cancellation and releases a blocked `execute`.
struct FixtureCancelHandle {
    events: Arc<Mutex<Vec<Event>>>,
    gate: Arc<ExecuteGate>,
}

impl CancelHandle for FixtureCancelHandle {
    fn try_cancel(&self) -> Result<()> {
        self.events.lock().unwrap().push(Event::Cancel);
        self.gate.state.lock().unwrap().cancelled = true;
        self.gate.changed.notify_all();
        Ok(())
    }
}

fn unsupported<T>() -> Result<T> {
    Err(Error::with_message_and_status(
        "fixture operation is not implemented",
        Status::NotImplemented,
    ))
}

adbc_ffi::export_driver!(SqlExecutableQueryFixtureInit, FixtureDriver);
