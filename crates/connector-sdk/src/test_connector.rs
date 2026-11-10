//! An in-memory Connector that exercises the runtime and the C ABI in tests.
//!
//! It is registered only with the `test-connector` feature and never ships in
//! a product build. Its catalog is described on [`TestCatalog`]. A plan's
//! table binding selects what its read does:
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
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, UNIX_EPOCH},
};

use crate::{
    CatalogReader, Column, ColumnType, Component, ConnectionOptions, ConnectorDescriptor,
    ConnectorError, ConnectorExecutor, ConnectorLibrary, ConnectorPlanner, Constraint,
    ConstraintKind, DataSource, DataSourceAccess, DataSourceSession, DiagnosticEntry,
    DiagnosticValue, ErrorKind, ExecutionCancel, ExecutionOutput, LibrarySettings, Listed,
    LogicalPlan, LogicalRelation, NameFilter, NamespacePath, OptionDescriptor, OptionValue,
    OptionValueType, PlannedPushdown, PlanningError, PortableQuery, PushdownResult, Reported,
    SourceTableMetadata, TableEntry, TableKind, TableMetadata, TableRecord, TableReference,
    UniqueKind, UnsupportedReason,
};
use analytics_types::{AnalyticsType, ArrayType, DecimalType};
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
            catalog: TestCatalog {
                probe: Arc::clone(&self.probe),
            },
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

impl Stop {
    /// Blocks until the read is stopped and returns the source's error.
    fn wait(&self) -> ConnectorError {
        let mut stopped = self
            .stopped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*stopped {
            stopped = self
                .signal
                .wait(stopped)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        ConnectorError::new(ErrorKind::DataSource, false, "the test source stopped")
    }
}

/// A Data Source error carrying a source code, a message, and diagnostics.
fn source_failure() -> ConnectorError {
    ConnectorError::new(ErrorKind::DataSource, true, "the test source failed")
        .with_data_source_error(
            Some("40001".to_owned()),
            Some("could not serialize\0access".to_owned()),
        )
        .with_diagnostics([
            DiagnosticEntry::new("vendor_code", DiagnosticValue::I32(-7)),
            DiagnosticEntry::new("detail", DiagnosticValue::Utf8("first".to_owned())),
            DiagnosticEntry::new("detail", DiagnosticValue::Bytes(vec![0, 1, 2])),
        ])
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
            Behavior::Block => Poll::Ready(Some(Err(this.stop.wait()))),
            Behavior::Fail => Poll::Ready(Some(Err(source_failure()))),
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

/// The test Data Source's catalog: a fixed fixture plus names that select a
/// behavior.
///
/// Namespaces `["test", "public"]` and `["test", "sales"]` are listed, and
/// `["test", "restricted"]` cannot be read. Filters match a namespace's last
/// name. Tables:
///
/// | Table (last name) | Listed in | Table metadata request |
/// | --- | --- | --- |
/// | `orders` | `public`, as a table | Complete: every column type state and constraint kind |
/// | `order_view` | `public`, as a view | Complete, with nothing offered beyond columns |
/// | `events` | `sales`, as another kind | Incomplete: every item that can be missing is missing |
/// | `missing` | — | Does not exist |
/// | `secret` | — | Could not be read |
///
/// A name `block`, `fail`, or `panic` in a filter or a table metadata request
/// makes the read, after the items before it, block until stopped, fail with
/// a Data Source error, or panic.
pub struct TestCatalog {
    probe: Arc<Probe>,
}

/// The test source metadata: which fixture table was read.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TestTable {
    pub table: Vec<String>,
    pub fixture: TestFixture,
}

/// The fixture tables that exist and can be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestFixture {
    Orders,
    OrderView,
    Events,
}

impl SourceTableMetadata for TestTable {
    fn to_table_metadata(&self) -> TableMetadata {
        let table = TableReference::new(self.table.clone()).expect("fixture names are non-empty");
        match self.fixture {
            TestFixture::Orders => orders(table),
            TestFixture::OrderView => TableMetadata::new(
                table,
                TableKind::View,
                Reported::NotOffered,
                vec![column(
                    "id",
                    1,
                    Reported::NotOffered,
                    ColumnType::Mapped {
                        data_source_type: "int8".to_owned(),
                        analytics_type: AnalyticsType::BigInt,
                    },
                )],
                false,
                Reported::NotOffered,
            ),
            TestFixture::Events => TableMetadata::new(
                table,
                TableKind::Other("partitioned table".to_owned()),
                Reported::Missing {
                    reason: "the table identifier is not readable".to_owned(),
                },
                vec![column(
                    "payload",
                    1,
                    Reported::Missing {
                        reason: "the column identifier is not readable".to_owned(),
                    },
                    ColumnType::Unread {
                        reason: "the column type is not readable".to_owned(),
                    },
                )],
                true,
                Reported::Missing {
                    reason: "the constraints are not readable".to_owned(),
                },
            ),
        }
    }
}

fn column(name: &str, ordinal: u32, source_id: Reported<String>, ty: ColumnType) -> Column {
    Column {
        name: name.to_owned(),
        ordinal,
        nullable: ordinal != 1,
        source_id,
        ty,
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn orders(table: TableReference) -> TableMetadata {
    let mapped = |data_source_type: &str, analytics_type| ColumnType::Mapped {
        data_source_type: data_source_type.to_owned(),
        analytics_type,
    };
    let id = |value: &str| Reported::Value(value.to_owned());
    TableMetadata::new(
        table,
        TableKind::Table,
        id("16384"),
        vec![
            column("id", 1, id("1"), mapped("int8", AnalyticsType::BigInt)),
            column(
                "amount",
                2,
                id("2"),
                mapped(
                    "numeric(10,2)",
                    AnalyticsType::Decimal(DecimalType::new(10, 2).expect("a valid decimal")),
                ),
            ),
            column("reference", 3, id("3"), mapped("uuid", AnalyticsType::Uuid)),
            column(
                "tags",
                4,
                id("4"),
                mapped(
                    "_text",
                    AnalyticsType::Array(ArrayType::new(AnalyticsType::Text, true)),
                ),
            ),
            column(
                "area",
                5,
                id("5"),
                ColumnType::Unsupported {
                    data_source_type: "geometry".to_owned(),
                    reason: UnsupportedReason::NotMapped,
                },
            ),
        ],
        false,
        Reported::Value(vec![
            Constraint {
                name: "orders_pkey".to_owned(),
                enforced: true,
                kind: ConstraintKind::PrimaryKey {
                    columns: names(&["id"]),
                },
            },
            Constraint {
                name: "orders_reference_key".to_owned(),
                enforced: true,
                kind: ConstraintKind::Unique {
                    columns: names(&["reference"]),
                    kind: UniqueKind::Index,
                    condition: Some("(amount > 0)".to_owned()),
                },
            },
            Constraint {
                name: "orders_customer_fkey".to_owned(),
                enforced: false,
                kind: ConstraintKind::ForeignKey {
                    columns: names(&["id"]),
                    referenced_table: TableReference::new(names(&["test", "public", "customers"]))
                        .expect("fixture names are non-empty"),
                    referenced_columns: names(&["customer_id"]),
                },
            },
            Constraint {
                name: "orders_amount_check".to_owned(),
                enforced: true,
                kind: ConstraintKind::Check {
                    expression: "(amount >= 0)".to_owned(),
                },
            },
        ]),
    )
}

/// One step of a catalog read.
enum CatalogStep<T> {
    Item(T),
    Block,
    Fail,
    Panic,
}

/// The step a behavior name selects, if it is one.
fn behavior_step<T>(name: &str) -> Option<CatalogStep<T>> {
    match name {
        "block" => Some(CatalogStep::Block),
        "fail" => Some(CatalogStep::Fail),
        "panic" => Some(CatalogStep::Panic),
        _ => None,
    }
}

fn matches(filter: &NameFilter, name: &str) -> bool {
    filter.as_ref().is_none_or(|names| names.contains(name))
}

/// The behavior steps a filter selects, after the listed items.
fn filter_steps<T>(filter: &NameFilter) -> impl Iterator<Item = CatalogStep<T>> + '_ {
    filter
        .iter()
        .flatten()
        .filter_map(|name| behavior_step(name))
}

impl TestCatalog {
    fn read<T: Send + 'static>(
        &self,
        steps: impl IntoIterator<Item = CatalogStep<T>>,
    ) -> ExecutionOutput<T> {
        self.probe.sent.fetch_add(1, Ordering::SeqCst);
        let stop = Arc::new(Stop::default());
        let stream = CatalogStream {
            steps: steps.into_iter().collect(),
            probe: Arc::clone(&self.probe),
            stop: Arc::clone(&stop),
        };
        let cancel = TestCancel {
            probe: Arc::clone(&self.probe),
            stop,
        };
        ExecutionOutput::new(Box::pin(stream), Arc::new(cancel))
    }
}

const PUBLIC: [&str; 2] = ["test", "public"];
const SALES: [&str; 2] = ["test", "sales"];
const RESTRICTED: [&str; 2] = ["test", "restricted"];

impl CatalogReader for TestCatalog {
    type Table = TestTable;

    fn list_namespaces(
        &self,
        filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        let mut steps: Vec<_> = [PUBLIC, SALES]
            .into_iter()
            .filter(|namespace| matches(filter, namespace[1]))
            .map(|namespace| {
                CatalogStep::Item(Listed::Item(NamespacePath {
                    names: names(&namespace),
                }))
            })
            .collect();
        if matches(filter, RESTRICTED[1]) {
            steps.push(CatalogStep::Item(Listed::Unreadable {
                scope: NamespacePath {
                    names: names(&RESTRICTED),
                },
                reason: "permission denied for the namespace".to_owned(),
            }));
        }
        steps.extend(filter_steps(filter));
        Ok(self.read(steps))
    }

    fn list_tables(
        &self,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        let fixture = [
            (PUBLIC, "orders", TableKind::Table),
            (PUBLIC, "order_view", TableKind::View),
            (
                SALES,
                "events",
                TableKind::Other("partitioned table".to_owned()),
            ),
        ];
        let mut steps: Vec<_> = fixture
            .into_iter()
            .filter(|(namespace, name, _)| {
                matches(namespaces, namespace[1]) && matches(tables, name)
            })
            .map(|(namespace, name, kind)| {
                let mut path = names(&namespace);
                path.push(name.to_owned());
                CatalogStep::Item(Listed::Item(TableEntry {
                    table: TableReference::new(path).expect("fixture names are non-empty"),
                    kind,
                }))
            })
            .collect();
        if matches(namespaces, RESTRICTED[1]) {
            steps.push(CatalogStep::Item(Listed::Unreadable {
                scope: NamespacePath {
                    names: names(&RESTRICTED),
                },
                reason: "permission denied for the namespace".to_owned(),
            }));
        }
        steps.extend(filter_steps(namespaces));
        steps.extend(filter_steps(tables));
        Ok(self.read(steps))
    }

    fn read_tables(
        &self,
        tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<TestTable>>, ConnectorError> {
        let steps: Vec<_> = tables
            .iter()
            .map(|table| {
                let name = table.names().last().map_or("", String::as_str);
                let read = |fixture| TableRecord::Read {
                    metadata: TestTable {
                        table: table.names().to_vec(),
                        fixture,
                    },
                    read_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                };
                behavior_step(name).unwrap_or_else(|| {
                    CatalogStep::Item(match name {
                        "orders" => read(TestFixture::Orders),
                        "order_view" => read(TestFixture::OrderView),
                        "events" => read(TestFixture::Events),
                        "secret" => TableRecord::Unavailable {
                            table: table.clone(),
                            reason: "permission denied for the table".to_owned(),
                        },
                        _ => TableRecord::NotFound {
                            table: table.clone(),
                        },
                    })
                })
            })
            .collect();
        Ok(self.read(steps))
    }
}

/// A catalog read's items, produced one per pull.
struct CatalogStream<T> {
    steps: VecDeque<CatalogStep<T>>,
    probe: Arc<Probe>,
    stop: Arc<Stop>,
}

impl<T> Unpin for CatalogStream<T> {}

impl<T> Stream for CatalogStream<T> {
    type Item = Result<T, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        Poll::Ready(this.steps.pop_front().map(|step| match step {
            CatalogStep::Item(item) => Ok(item),
            CatalogStep::Block => Err(this.stop.wait()),
            CatalogStep::Fail => Err(source_failure()),
            CatalogStep::Panic => panic!("the test connector panicked in a catalog read"),
        }))
    }
}

impl<T> Drop for CatalogStream<T> {
    fn drop(&mut self) {
        self.probe.released.fetch_add(1, Ordering::SeqCst);
    }
}
