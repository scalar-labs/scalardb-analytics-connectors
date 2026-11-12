use std::{
    collections::VecDeque,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, SystemTime},
};

use analytics_types::AnalyticsType;
use connector_sdk::{
    Column, ColumnAttribute, ColumnChange, ColumnType, ConnectorError, Constraint, ConstraintKind,
    Difference, ErrorKind, ExecutionCancel, ExecutionOutput, Listed, NameFilter, NamespacePath,
    Reported, SourceStream, SourceTableMetadata, TableEntry, TableKind, TableMetadata, TableRecord,
    TableReference, UniqueKind, UnsupportedReason, start_source_read,
};
use futures_core::Stream;
use serde::{Deserialize, Serialize};

#[test]
fn table_metadata_order_does_not_depend_on_the_source_order() {
    let table = source_table();
    let mut reordered = source_table();
    reordered.columns.reverse();
    reordered.constraints.reverse();

    let metadata = table.to_table_metadata();

    assert_eq!(metadata, reordered.to_table_metadata());
    let ordinals: Vec<u32> = metadata
        .columns()
        .iter()
        .map(|column| column.ordinal)
        .collect();
    assert_eq!(ordinals, vec![1, 2, 3, 4, 5]);
    let Reported::Value(constraints) = metadata.constraints() else {
        panic!("the constraints were read");
    };
    let names: Vec<&str> = constraints
        .iter()
        .map(|constraint| constraint.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["orders_pkey", "orders_code_key", "orders_total_check"]
    );
}

#[test]
fn unsupported_columns_keep_their_type_and_reason() {
    let metadata = source_table().to_table_metadata();

    let types: Vec<&ColumnType> = metadata.columns().iter().map(|column| &column.ty).collect();
    assert_eq!(
        types,
        vec![
            &ColumnType::Mapped {
                data_source_type: "int8".to_owned(),
                analytics_type: AnalyticsType::BigInt,
            },
            &ColumnType::Unsupported {
                data_source_type: "money".to_owned(),
                reason: UnsupportedReason::NotMapped,
            },
            &ColumnType::Unsupported {
                data_source_type: "interval".to_owned(),
                reason: UnsupportedReason::MappingNotImplemented,
            },
            &ColumnType::Unsupported {
                data_source_type: "numeric(1000,0)".to_owned(),
                reason: UnsupportedReason::ParameterOutOfRange,
            },
            &ColumnType::Mapped {
                data_source_type: "text".to_owned(),
                analytics_type: AnalyticsType::Text,
            },
        ]
    );
}

#[test]
fn completeness_follows_missing_items_but_not_items_never_offered() {
    assert!(source_table().to_table_metadata().is_complete());

    let mut never_offered = source_table();
    never_offered.constraints_offered = false;
    never_offered.oid = None;
    assert!(never_offered.to_table_metadata().is_complete());

    let mut unread_type = source_table();
    unread_type.columns[0].data_type = None;
    assert!(!unread_type.to_table_metadata().is_complete());

    let mut missing_identifier = source_table();
    missing_identifier.oid_readable = false;
    assert!(!missing_identifier.to_table_metadata().is_complete());

    let mut hidden_columns = source_table();
    hidden_columns.catalog_hides_columns = true;
    assert!(!hidden_columns.to_table_metadata().is_complete());
}

#[test]
fn source_metadata_is_a_connector_defined_json_document() {
    let document = serde_json::to_value(source_table()).unwrap();

    assert_eq!(document["relkind"], "r");
    assert_eq!(document["columns"][1]["data_type"], "money");
}

#[test]
fn stored_source_metadata_is_compared_through_the_connector_type() {
    let stored = serde_json::to_string(&source_table()).unwrap();
    let mut changed = source_table();
    changed.columns[4].not_null = true;

    let restored: FakeTable = serde_json::from_str(&stored).unwrap();

    assert_eq!(restored, source_table());
    assert_ne!(restored, changed);
    let changes = restored
        .to_table_metadata()
        .compare(&changed.to_table_metadata());
    let attributes: Vec<_> = changes
        .columns
        .iter()
        .map(|change| match change {
            ColumnChange::Changed { attributes, .. } => attributes.as_slice(),
            other => panic!("the column stays matched: {other:?}"),
        })
        .collect();
    assert_eq!(
        attributes,
        vec![[Difference::Changed(ColumnAttribute::Nullable)].as_slice()]
    );
}

#[test]
fn table_records_stream_through_a_source_read() {
    let reader = FakeReader::default();
    let tables = [table_reference("orders"), table_reference("missing")];
    let mut stream = start_source_read(None, || reader.table_metadata(&tables)).unwrap();

    let Poll::Ready(Some(Ok(TableRecord::Read {
        metadata: table, ..
    }))) = pull(&mut stream)
    else {
        panic!("the first table was read");
    };
    assert_eq!(
        table.to_table_metadata().table(),
        &table_reference("orders")
    );
    assert_eq!(
        pull(&mut stream),
        Poll::Ready(Some(Ok(TableRecord::NotFound {
            table: table_reference("missing"),
        })))
    );
    assert_eq!(
        pull(&mut stream),
        Poll::Ready(Some(Ok(TableRecord::Unavailable {
            table: table_reference("secret"),
            reason: "permission denied".to_owned(),
        })))
    );
    assert_eq!(pull(&mut stream), Poll::Ready(None));
}

#[test]
fn catalog_reads_stop_on_cancel_and_expiry() {
    let reader = FakeReader::default();
    let filter: NameFilter = None;

    let mut namespaces = start_source_read(None, || reader.namespace_list(&filter)).unwrap();
    namespaces.cancel_handle().cancel().unwrap();
    assert!(matches!(
        pull(&mut namespaces),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Cancellation
    ));
    assert_eq!(pull(&mut namespaces), Poll::Ready(None));

    let mut tables = start_source_read(None, || reader.table_list(&filter, &filter)).unwrap();
    tables.cancel_handle().expire().unwrap();
    assert!(matches!(
        pull(&mut tables),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Timeout
    ));

    let requested = [table_reference("orders")];
    let mut records = start_source_read(None, || reader.table_metadata(&requested)).unwrap();
    records.cancel_handle().cancel().unwrap();
    assert!(matches!(
        pull(&mut records),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Cancellation
    ));

    assert_eq!(reader.cancel_calls.load(Ordering::SeqCst), 3);
}

/// A PostgreSQL-like catalog row set for one table, owned by the test connector.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct FakeTable {
    name: String,
    relkind: String,
    oid: Option<u32>,
    oid_readable: bool,
    catalog_hides_columns: bool,
    columns: Vec<FakeColumn>,
    constraints_offered: bool,
    constraints: Vec<FakeConstraint>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct FakeColumn {
    name: String,
    attnum: u32,
    not_null: bool,
    data_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct FakeConstraint {
    name: String,
    contype: char,
    columns: Vec<String>,
    expression: Option<String>,
}

impl SourceTableMetadata for FakeTable {
    fn to_table_metadata(&self) -> TableMetadata {
        let source_id = match (self.oid_readable, self.oid) {
            (false, _) => Reported::Missing {
                reason: "pg_class is not readable".to_owned(),
            },
            (true, Some(oid)) => Reported::Value(oid.to_string()),
            (true, None) => Reported::NotOffered,
        };
        let columns = self
            .columns
            .iter()
            .map(|column| Column {
                name: column.name.to_owned(),
                ordinal: column.attnum,
                nullable: !column.not_null,
                source_id: Reported::Value(column.attnum.to_string()),
                ty: map_type(column.data_type.as_deref()),
            })
            .collect();
        let constraints = if self.constraints_offered {
            Reported::Value(self.constraints.iter().map(to_constraint).collect())
        } else {
            Reported::NotOffered
        };
        TableMetadata::new(
            table_reference(&self.name),
            match self.relkind.as_str() {
                "r" => TableKind::Table,
                "v" => TableKind::View,
                other => TableKind::Other(other.to_owned()),
            },
            source_id,
            columns,
            self.catalog_hides_columns,
            constraints,
        )
    }
}

/// The test connector's type mapping.
fn map_type(data_type: Option<&str>) -> ColumnType {
    let Some(data_type) = data_type else {
        return ColumnType::Unread {
            reason: "format_type returned no type".to_owned(),
        };
    };
    let mapped = |analytics_type| ColumnType::Mapped {
        data_source_type: data_type.to_owned(),
        analytics_type,
    };
    let unsupported = |reason| ColumnType::Unsupported {
        data_source_type: data_type.to_owned(),
        reason,
    };
    match data_type {
        "int8" => mapped(AnalyticsType::BigInt),
        "text" => mapped(AnalyticsType::Text),
        // The type mapping maps interval to Interval, but this test connector's
        // driver returns it without separating months and days.
        "interval" => unsupported(UnsupportedReason::MappingNotImplemented),
        "numeric(1000,0)" => unsupported(UnsupportedReason::ParameterOutOfRange),
        _ => unsupported(UnsupportedReason::NotMapped),
    }
}

fn to_constraint(constraint: &FakeConstraint) -> Constraint {
    let columns = constraint.columns.clone();
    let kind = match constraint.contype {
        'p' => ConstraintKind::PrimaryKey { columns },
        'u' => ConstraintKind::Unique {
            columns,
            kind: UniqueKind::Constraint,
            condition: None,
        },
        _ => ConstraintKind::Check {
            expression: constraint.expression.clone().unwrap_or_default(),
        },
    };
    Constraint {
        name: constraint.name.clone(),
        enforced: true,
        kind,
    }
}

fn source_table() -> FakeTable {
    let column = |name: &str, attnum, data_type: &str| FakeColumn {
        name: name.to_owned(),
        attnum,
        not_null: attnum == 1,
        data_type: Some(data_type.to_owned()),
    };
    FakeTable {
        name: "orders".to_owned(),
        relkind: "r".to_owned(),
        oid: Some(16384),
        oid_readable: true,
        catalog_hides_columns: false,
        columns: vec![
            column("id", 1, "int8"),
            column("price", 2, "money"),
            column("lead_time", 3, "interval"),
            column("total", 4, "numeric(1000,0)"),
            column("code", 5, "text"),
        ],
        constraints_offered: true,
        constraints: vec![
            FakeConstraint {
                name: "orders_total_check".to_owned(),
                contype: 'c',
                columns: vec![],
                expression: Some("total > 0".to_owned()),
            },
            FakeConstraint {
                name: "orders_code_key".to_owned(),
                contype: 'u',
                columns: vec!["code".to_owned()],
                expression: None,
            },
            FakeConstraint {
                name: "orders_pkey".to_owned(),
                contype: 'p',
                columns: vec!["id".to_owned()],
                expression: None,
            },
        ],
    }
}

fn table_reference(name: &str) -> TableReference {
    TableReference::new(vec!["public".to_owned(), name.to_owned()]).unwrap()
}

fn pull<T>(stream: &mut SourceStream<T>) -> Poll<Option<Result<T, ConnectorError>>> {
    let mut context = Context::from_waker(Waker::noop());
    Stream::poll_next(Pin::new(stream), &mut context)
}

/// A catalog reader that returns fixed items and counts cancellations.
#[derive(Default)]
struct FakeReader {
    cancel_calls: Arc<AtomicUsize>,
}

impl FakeReader {
    fn output<T: Send + Unpin + 'static>(&self, items: Vec<T>) -> ExecutionOutput<T> {
        let cancel = Arc::new(CountingCancel(Arc::clone(&self.cancel_calls)));
        ExecutionOutput::new(Box::pin(ItemStream(items.into())), cancel)
    }
}

/// The three catalog reads, shaped as a Connector's catalog returns them.
impl FakeReader {
    fn namespace_list(
        &self,
        _filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError> {
        Ok(self.output(vec![Listed::Item(NamespacePath {
            names: vec!["public".to_owned()],
        })]))
    }

    fn table_list(
        &self,
        _namespaces: &NameFilter,
        _tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError> {
        Ok(self.output(vec![Listed::Item(TableEntry {
            table: table_reference("orders"),
            kind: TableKind::Table,
        })]))
    }

    fn table_metadata(
        &self,
        _tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<FakeTable>>, ConnectorError> {
        Ok(self.output(vec![
            TableRecord::Read {
                metadata: source_table(),
                read_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            },
            TableRecord::NotFound {
                table: table_reference("missing"),
            },
            TableRecord::Unavailable {
                table: table_reference("secret"),
                reason: "permission denied".to_owned(),
            },
        ]))
    }
}

struct CountingCancel(Arc<AtomicUsize>);

impl ExecutionCancel for CountingCancel {
    fn cancel(&self) -> Result<(), ConnectorError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ItemStream<T>(VecDeque<T>);

impl<T: Unpin> Stream for ItemStream<T> {
    type Item = Result<T, ConnectorError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.pop_front().map(Ok))
    }
}
