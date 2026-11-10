//! An in-memory Connector that exercises the C ABI in this crate's tests.
//!
//! It is registered only with the `test-connector` feature and never ships in
//! a product build. A plan's table binding selects what its read does:
//!
//! | Binding | Read behavior |
//! | --- | --- |
//! | `batches`, `<n>` | Returns `n` batches of one row each |
//! | `block` | Blocks the first pull until the read is cancelled |
//! | `fail` | Fails the first pull with a Data Source error carrying details |
//! | `break-connection` | Fails the first pull with a broken connection |
//! | `panic` | Panics in the first pull |
//! | `break-connection-planning` | Planning fails with a broken connection |
//! | `panic-planning` | Planning panics |
//! | anything else | Planning returns unsupported |
//!
//! Every plan's output columns must be 64-bit integers.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use crate::{
    CatalogReader, Component, ConnectionOptions, ConnectorDescriptor, ConnectorError,
    ConnectorExecutor, ConnectorLibrary, ConnectorPlanner, DataSource, DataSourceAccess,
    DataSourceSession, DiagnosticEntry, DiagnosticValue, ErrorKind, ExecutionCancel,
    ExecutionOutput, LibrarySettings, Listed, LogicalPlan, LogicalRelation, NameFilter,
    NamespacePath, OptionDescriptor, OptionValue, OptionValueType, PlannedPushdown, PlanningError,
    PortableQuery, PushdownResult, SourceTableMetadata, TableEntry, TableMetadata, TableRecord,
    TableReference,
};
use arrow_array::{ArrayRef, Int64Array, RecordBatch};
use arrow_schema::{Schema, SchemaRef};
use futures_core::Stream;

/// What the test Data Source observed, shared with the test through its label.
#[derive(Debug, Default)]
pub struct Probe {
    /// Queries sent to the Data Source.
    pub sent: AtomicUsize,
    /// Reads stopped by cancellation or timeout.
    pub cancelled: AtomicUsize,
    /// Data Source streams released.
    pub released: AtomicUsize,
}

/// Returns the probe of the Data Sources opened with the `label` option.
pub fn probe(label: &str) -> Arc<Probe> {
    static PROBES: OnceLock<Mutex<HashMap<String, Arc<Probe>>>> = OnceLock::new();
    let mut probes = PROBES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(probes.entry(label.to_owned()).or_default())
}

pub struct TestConnectorLibrary;

impl ConnectorLibrary for TestConnectorLibrary {
    type DataSource = TestDataSource;

    fn descriptor() -> ConnectorDescriptor {
        ConnectorDescriptor {
            name: "test".to_owned(),
            version: "0.1.0".to_owned(),
            connectors: vec!["test".to_owned()],
            options: vec![
                OptionDescriptor {
                    key: "label".to_owned(),
                    secret: false,
                    required: true,
                    value_type: OptionValueType::String,
                    default: None,
                    description: "Names the probe that records what the source observed."
                        .to_owned(),
                },
                OptionDescriptor {
                    key: "token".to_owned(),
                    secret: true,
                    required: false,
                    value_type: OptionValueType::String,
                    default: None,
                    description: "A credential the source never reveals.".to_owned(),
                },
            ],
            access: DataSourceAccess::Direct,
        }
    }

    fn new(_settings: &LibrarySettings) -> Result<Self, ConnectorError> {
        Ok(Self)
    }

    fn open_data_source(
        &self,
        _connector: &str,
        options: ConnectionOptions,
        _connection_timeout: Option<Duration>,
    ) -> Result<TestDataSource, ConnectorError> {
        let Some(OptionValue::Plain(label)) = options.get("label") else {
            unreachable!("the descriptor requires a plain label");
        };
        Ok(TestDataSource {
            probe: probe(label),
        })
    }
}

pub struct TestDataSource {
    probe: Arc<Probe>,
}

impl DataSource for TestDataSource {
    type Session = TestSession;

    fn open_session(&self) -> Result<TestSession, ConnectorError> {
        Ok(TestSession {
            planner: TestPlanner,
            executor: TestExecutor {
                probe: Arc::clone(&self.probe),
            },
            catalog: TestCatalog,
        })
    }
}

pub struct TestSession {
    planner: TestPlanner,
    executor: TestExecutor,
    catalog: TestCatalog,
}

impl DataSourceSession for TestSession {
    type Query = TestQuery;
    type Catalog = TestCatalog;
    type Planner = TestPlanner;
    type Executor = TestExecutor;

    fn catalog(&self) -> &TestCatalog {
        &self.catalog
    }

    fn planner(&self) -> &TestPlanner {
        &self.planner
    }

    fn executor(&self) -> &TestExecutor {
        &self.executor
    }

    fn adbc_driver(&self) -> Option<Component> {
        None
    }
}

/// What a planned read does at the Data Source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behavior {
    Batches(u32),
    Block,
    Fail,
    BreakConnection,
    Panic,
}

/// The test Connector's executable query: a behavior and the output schema.
#[derive(Clone, Debug, PartialEq)]
pub struct TestQuery {
    behavior: Behavior,
    schema: Schema,
}

impl PortableQuery for TestQuery {
    fn to_bytes(&self) -> Vec<u8> {
        let behavior = match self.behavior {
            Behavior::Batches(count) => format!("batches {count}"),
            Behavior::Block => "block".to_owned(),
            Behavior::Fail => "fail".to_owned(),
            Behavior::BreakConnection => "break-connection".to_owned(),
            Behavior::Panic => "panic".to_owned(),
        };
        let schema = serde_json::to_string(&self.schema).expect("a schema serializes");
        format!("{behavior}\n{schema}").into_bytes()
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError> {
        let invalid = || ConnectorError::new(ErrorKind::InvalidRequest, false, "not a test query");
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let (behavior, schema) = text.split_once('\n').ok_or_else(invalid)?;
        let behavior = match behavior.split_once(' ') {
            Some(("batches", count)) => Behavior::Batches(count.parse().map_err(|_| invalid())?),
            None if behavior == "block" => Behavior::Block,
            None if behavior == "fail" => Behavior::Fail,
            None if behavior == "break-connection" => Behavior::BreakConnection,
            None if behavior == "panic" => Behavior::Panic,
            _ => return Err(invalid()),
        };
        let schema = serde_json::from_str(schema).map_err(|_| invalid())?;
        Ok(Self { behavior, schema })
    }
}

pub struct TestPlanner;

impl ConnectorPlanner for TestPlanner {
    type Query = TestQuery;

    fn plan(&self, offered_plan: &LogicalPlan) -> Result<PushdownResult<TestQuery>, PlanningError> {
        let LogicalRelation::Read { table_binding, .. } = offered_plan.relation() else {
            return Ok(PushdownResult::Unsupported {
                reason: "the test connector plans only table reads".to_owned(),
            });
        };
        let behavior = match table_binding.components() {
            [name, count] if name == "batches" => {
                Behavior::Batches(count.parse().map_err(|_| {
                    ConnectorError::new(ErrorKind::InvalidRequest, false, "invalid batch count")
                })?)
            }
            [name] if name == "block" => Behavior::Block,
            [name] if name == "fail" => Behavior::Fail,
            [name] if name == "break-connection" => Behavior::BreakConnection,
            [name] if name == "panic" => Behavior::Panic,
            [name] if name == "panic-planning" => panic!("the test connector panicked in planning"),
            [name] if name == "break-connection-planning" => {
                return Err(PlanningError::Connector(ConnectorError::new(
                    ErrorKind::Connection,
                    true,
                    "the test connection broke while planning",
                )));
            }
            _ => {
                return Ok(PushdownResult::Unsupported {
                    reason: "the test connector does not know this table".to_owned(),
                });
            }
        };
        let query = TestQuery {
            behavior,
            schema: offered_plan.output_schema().to_arrow(),
        };
        let pushdown = PlannedPushdown::try_new(offered_plan, offered_plan.clone(), None, query)?;
        Ok(PushdownResult::Planned {
            pushdown: Box::new(pushdown),
        })
    }
}

pub struct TestExecutor {
    probe: Arc<Probe>,
}

impl ConnectorExecutor for TestExecutor {
    type Query = TestQuery;

    fn execute(&self, query: &TestQuery) -> Result<ExecutionOutput, ConnectorError> {
        self.probe.sent.fetch_add(1, Ordering::SeqCst);
        let stop = Arc::new(Stop::default());
        let stream = TestStream {
            behavior: query.behavior,
            produced: 0,
            schema: Arc::new(query.schema.clone()),
            probe: Arc::clone(&self.probe),
            stop: Arc::clone(&stop),
        };
        let cancel = TestCancel {
            probe: Arc::clone(&self.probe),
            stop,
        };
        Ok(ExecutionOutput::new(Box::pin(stream), Arc::new(cancel)))
    }
}

/// Whether the read was asked to stop, with a signal for a blocked pull.
#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    signal: Condvar,
}

struct TestCancel {
    probe: Arc<Probe>,
    stop: Arc<Stop>,
}

impl ExecutionCancel for TestCancel {
    fn cancel(&self) -> Result<(), ConnectorError> {
        let mut stopped = self
            .stop
            .stopped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*stopped {
            *stopped = true;
            self.probe.cancelled.fetch_add(1, Ordering::SeqCst);
        }
        self.stop.signal.notify_all();
        Ok(())
    }
}

struct TestStream {
    behavior: Behavior,
    produced: u32,
    schema: SchemaRef,
    probe: Arc<Probe>,
    stop: Arc<Stop>,
}

impl TestStream {
    fn batch(&self) -> RecordBatch {
        let columns = self
            .schema
            .fields()
            .iter()
            .map(|_| Arc::new(Int64Array::from(vec![i64::from(self.produced)])) as ArrayRef)
            .collect();
        RecordBatch::try_new(Arc::clone(&self.schema), columns)
            .expect("test plans have only 64-bit integer columns")
    }
}

impl Stream for TestStream {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.behavior {
            Behavior::Batches(count) if this.produced < count => {
                let batch = this.batch();
                this.produced += 1;
                Poll::Ready(Some(Ok(batch)))
            }
            Behavior::Batches(_) => Poll::Ready(None),
            Behavior::Block => {
                let mut stopped = this
                    .stop
                    .stopped
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while !*stopped {
                    stopped = this
                        .stop
                        .signal
                        .wait(stopped)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                Poll::Ready(Some(Err(ConnectorError::new(
                    ErrorKind::DataSource,
                    false,
                    "the test source stopped",
                ))))
            }
            Behavior::Fail => Poll::Ready(Some(Err(ConnectorError::new(
                ErrorKind::DataSource,
                true,
                "the test source failed",
            )
            .with_data_source_error(
                Some("40001".to_owned()),
                Some("could not serialize\0access".to_owned()),
            )
            .with_diagnostics([
                DiagnosticEntry::new("vendor_code", DiagnosticValue::I32(-7)),
                DiagnosticEntry::new("detail", DiagnosticValue::Utf8("first".to_owned())),
                DiagnosticEntry::new("detail", DiagnosticValue::Bytes(vec![0, 1, 2])),
            ])))),
            Behavior::BreakConnection => Poll::Ready(Some(Err(ConnectorError::new(
                ErrorKind::Connection,
                true,
                "the test connection broke",
            )))),
            Behavior::Panic => panic!("the test connector panicked in a pull"),
        }
    }
}

impl Drop for TestStream {
    fn drop(&mut self) {
        self.probe.released.fetch_add(1, Ordering::SeqCst);
    }
}

/// The test Connector has no catalog; metadata requests are not part of its tests.
pub struct TestCatalog;

/// Never constructed: the test catalog reads no tables.
#[derive(serde::Serialize)]
pub struct NoTable;

impl SourceTableMetadata for NoTable {
    fn to_table_metadata(&self) -> TableMetadata {
        unreachable!("the test catalog reads no tables")
    }
}

impl CatalogReader for TestCatalog {
    type Table = NoTable;

    fn list_namespaces(
        &self,
        _filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        Err(no_catalog())
    }

    fn list_tables(
        &self,
        _namespaces: &NameFilter,
        _tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        Err(no_catalog())
    }

    fn read_tables(
        &self,
        _tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<NoTable>>, ConnectorError> {
        Err(no_catalog())
    }
}

fn no_catalog() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::InvalidRequest,
        false,
        "the test connector has no catalog",
    )
}
