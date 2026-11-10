//! Metadata requests driven through the runtime with the in-memory test
//! Connector: their rows, and the calling conventions they share with reads.

use std::{
    collections::BTreeSet,
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

use analytics_types::{AnalyticsField, AnalyticsType, ArrayType, DecimalType};
use arrow_array::{
    Array, ArrayRef, RecordBatch, StructArray,
    cast::AsArray,
    types::{TimestampMicrosecondType, UInt32Type},
};
use connector_sdk::{
    ConnectorError, ErrorKind, LibrarySettings, NameFilter, SecretString, TableReference,
    runtime::{BuiltInConnector, Library, MetadataRequest, Operation, SessionState},
    test_connector::{Probe, TestConnectorLibrary, TestFixture, TestTable, probe},
};

const LIBRARY_VERSION: &str = "0.0.0-test";

fn session(label: &str) -> (Arc<SessionState>, Arc<Probe>) {
    let library = Library::initialize(
        &[BuiltInConnector::of::<TestConnectorLibrary>()],
        &LibrarySettings::default(),
        LIBRARY_VERSION,
    )
    .unwrap();
    let label = format!("metadata-{label}");
    let options = vec![("label".to_owned(), SecretString::new(label.clone()))];
    let data_source = library.open_data_source("test", options, None).unwrap();
    (data_source.open_session().unwrap(), probe(&label))
}

fn filter(names: &[&str]) -> NameFilter {
    Some(
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>(),
    )
}

fn tables(names: &[&str]) -> MetadataRequest {
    MetadataRequest::TableMetadata {
        tables: names
            .iter()
            .map(|name| {
                TableReference::new(vec![
                    "test".to_owned(),
                    "public".to_owned(),
                    (*name).to_owned(),
                ])
                .unwrap()
            })
            .collect(),
    }
}

fn pull_rows(operation: &Operation) -> Result<Vec<RecordBatch>, ConnectorError> {
    let mut rows = Vec::new();
    while let Some(batch) = operation.pull()? {
        assert_eq!(batch.schema(), operation.output_schema());
        assert_eq!(batch.num_rows(), 1, "each item is one row");
        rows.push(batch);
    }
    Ok(rows)
}

fn rows(session: &Arc<SessionState>, request: MetadataRequest) -> Vec<RecordBatch> {
    pull_rows(&session.start_metadata(request, None).unwrap()).unwrap()
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached in time");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Reads the text at row 0 of a column; `None` when null.
fn text(array: &ArrayRef) -> Option<String> {
    let array = array.as_string::<i32>();
    array.is_valid(0).then(|| array.value(0).to_owned())
}

/// Reads the name path at row 0 of a column; `None` when null.
fn path(array: &ArrayRef) -> Option<Vec<String>> {
    let array = array.as_list::<i32>();
    array.is_valid(0).then(|| {
        array
            .value(0)
            .as_string::<i32>()
            .iter()
            .map(|name| name.unwrap().to_owned())
            .collect()
    })
}

fn column<'a>(batch: &'a RecordBatch, name: &str) -> &'a ArrayRef {
    batch.column_by_name(name).unwrap()
}

fn field<'a>(array: &'a StructArray, name: &str) -> &'a ArrayRef {
    array.column_by_name(name).unwrap()
}

/// Slices row `index` of an array so the row-0 readers apply.
fn row(array: &ArrayRef, index: usize) -> ArrayRef {
    array.slice(index, 1)
}

/// The state, value, and reason of a reported text item at row 0.
fn reported(array: &ArrayRef) -> (String, Option<String>, Option<String>) {
    let array = array.as_struct();
    (
        text(field(array, "state")).unwrap(),
        text(field(array, "value")),
        text(field(array, "reason")),
    )
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn each_request_returns_its_fixed_schema_before_the_source_is_queried() {
    let (session, probe) = session("schema");
    let requests = [
        (
            MetadataRequest::NamespaceList { namespaces: None },
            vec!["state", "namespace", "scope", "reason"],
        ),
        (
            MetadataRequest::TableList {
                namespaces: None,
                tables: None,
            },
            vec!["state", "table", "kind", "source_kind", "scope", "reason"],
        ),
        (
            tables(&["orders"]),
            vec![
                "state",
                "table",
                "reason",
                "kind",
                "source_kind",
                "source_id",
                "columns",
                "columns_may_be_hidden",
                "constraints",
                "source_metadata",
                "retrieval",
            ],
        ),
    ];
    for (request, names) in requests {
        let operation = session.start_metadata(request, None).unwrap();
        let schema = operation.output_schema();
        let fields: Vec<&str> = schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        assert_eq!(fields, names);
        operation.release();
    }
    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
}

#[test]
fn a_namespace_list_returns_listed_and_unreadable_ranges_matching_names_exactly() {
    let (session, _) = session("namespaces");

    let all = rows(
        &session,
        MetadataRequest::NamespaceList { namespaces: None },
    );

    let listed: Vec<_> = all
        .iter()
        .map(|batch| {
            (
                text(column(batch, "state")).unwrap(),
                path(column(batch, "namespace")),
                path(column(batch, "scope")),
                text(column(batch, "reason")),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            (
                "listed".to_owned(),
                Some(strings(&["test", "public"])),
                None,
                None
            ),
            (
                "listed".to_owned(),
                Some(strings(&["test", "sales"])),
                None,
                None
            ),
            (
                "unreadable".to_owned(),
                None,
                Some(strings(&["test", "restricted"])),
                Some("permission denied for the namespace".to_owned()),
            ),
        ]
    );
    let sales = rows(
        &session,
        MetadataRequest::NamespaceList {
            namespaces: filter(&["sales"]),
        },
    );
    assert_eq!(sales.len(), 1);
    assert!(
        rows(
            &session,
            MetadataRequest::NamespaceList {
                namespaces: filter(&["Sales"]),
            },
        )
        .is_empty()
    );
}

#[test]
fn a_table_list_returns_each_table_kind_and_unreadable_ranges() {
    let (session, _) = session("tables");

    let all = rows(
        &session,
        MetadataRequest::TableList {
            namespaces: None,
            tables: None,
        },
    );

    let listed: Vec<_> = all
        .iter()
        .map(|batch| {
            (
                text(column(batch, "state")).unwrap(),
                path(column(batch, "table")),
                text(column(batch, "kind")),
                text(column(batch, "source_kind")),
                path(column(batch, "scope")),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            (
                "listed".to_owned(),
                Some(strings(&["test", "public", "orders"])),
                Some("table".to_owned()),
                None,
                None,
            ),
            (
                "listed".to_owned(),
                Some(strings(&["test", "public", "order_view"])),
                Some("view".to_owned()),
                None,
                None,
            ),
            (
                "listed".to_owned(),
                Some(strings(&["test", "sales", "events"])),
                Some("other".to_owned()),
                Some("partitioned table".to_owned()),
                None,
            ),
            (
                "unreadable".to_owned(),
                None,
                None,
                None,
                Some(strings(&["test", "restricted"])),
            ),
        ]
    );
    let orders = rows(
        &session,
        MetadataRequest::TableList {
            namespaces: filter(&["public"]),
            tables: filter(&["orders"]),
        },
    );
    assert_eq!(orders.len(), 1);
}

#[test]
fn table_metadata_returns_one_row_per_table_in_each_state() {
    let (session, _) = session("states");

    let all = rows(
        &session,
        tables(&["orders", "order_view", "events", "missing", "secret"]),
    );

    let states: Vec<_> = all
        .iter()
        .map(|batch| {
            (
                text(column(batch, "state")).unwrap(),
                path(column(batch, "table")).unwrap().pop().unwrap(),
                text(column(batch, "reason")),
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            ("complete".to_owned(), "orders".to_owned(), None),
            ("complete".to_owned(), "order_view".to_owned(), None),
            ("incomplete".to_owned(), "events".to_owned(), None),
            ("does_not_exist".to_owned(), "missing".to_owned(), None),
            (
                "could_not_be_read".to_owned(),
                "secret".to_owned(),
                Some("permission denied for the table".to_owned()),
            ),
        ]
    );
    for unread in &all[3..] {
        for name in [
            "kind",
            "source_id",
            "columns",
            "columns_may_be_hidden",
            "constraints",
            "source_metadata",
        ] {
            assert!(column(unread, name).is_null(0), "{name} is absent");
        }
        let retrieval = column(unread, "retrieval").as_struct();
        assert!(field(retrieval, "read_at").is_null(0));
        assert_eq!(
            text(field(field(retrieval, "connector").as_struct(), "name")).as_deref(),
            Some("test")
        );
    }
}

#[test]
fn a_read_table_carries_columns_constraints_source_metadata_and_retrieval_info() {
    let (session, _) = session("orders");

    let orders = rows(&session, tables(&["orders"])).pop().unwrap();

    assert_eq!(text(column(&orders, "kind")).as_deref(), Some("table"));
    assert_eq!(
        reported(column(&orders, "source_id")),
        ("value".to_owned(), Some("16384".to_owned()), None)
    );
    assert!(
        !column(&orders, "columns_may_be_hidden")
            .as_boolean()
            .value(0)
    );

    let columns = column(&orders, "columns").as_list::<i32>().value(0);
    let columns = columns.as_struct();
    let names: Vec<_> = field(columns, "name")
        .as_string::<i32>()
        .iter()
        .map(|name| name.unwrap())
        .collect();
    assert_eq!(names, ["id", "amount", "reference", "tags", "area"]);
    let ordinals: Vec<u32> = field(columns, "ordinal")
        .as_primitive::<UInt32Type>()
        .values()
        .to_vec();
    assert_eq!(ordinals, [1, 2, 3, 4, 5]);
    assert!(!field(columns, "nullable").as_boolean().value(0));
    assert_eq!(
        reported(&row(field(columns, "source_id"), 1)),
        ("value".to_owned(), Some("2".to_owned()), None)
    );

    let types = field(columns, "type").as_struct();
    let expected = [
        AnalyticsType::BigInt,
        AnalyticsType::Decimal(DecimalType::new(10, 2).unwrap()),
        AnalyticsType::Uuid,
        AnalyticsType::Array(ArrayType::new(AnalyticsType::Text, true)),
    ];
    for (index, analytics_type) in expected.iter().enumerate() {
        assert_eq!(
            text(&row(field(types, "state"), index)).as_deref(),
            Some("mapped")
        );
        let bytes = field(types, "analytics_type")
            .as_binary::<i32>()
            .value(index);
        let schema = arrow_ipc::convert::try_schema_from_ipc_buffer(bytes).unwrap();
        let [decoded] = schema.fields().as_ref() else {
            panic!("the encoded schema has one field");
        };
        assert_eq!(decoded.name(), "");
        assert_eq!(
            &AnalyticsField::try_from_arrow(decoded).unwrap(),
            &AnalyticsField::new("", analytics_type.clone(), true)
        );
    }
    assert_eq!(
        text(&row(field(types, "data_source_type"), 1)).as_deref(),
        Some("numeric(10,2)")
    );
    assert_eq!(
        text(&row(field(types, "state"), 4)).as_deref(),
        Some("unsupported")
    );
    assert_eq!(
        text(&row(field(types, "data_source_type"), 4)).as_deref(),
        Some("geometry")
    );
    assert!(field(types, "analytics_type").is_null(4));
    assert_eq!(
        text(&row(field(types, "unsupported_reason"), 4)).as_deref(),
        Some("not_mapped")
    );

    let constraints = column(&orders, "constraints").as_struct();
    assert_eq!(text(field(constraints, "state")).as_deref(), Some("value"));
    let constraints = field(constraints, "value").as_list::<i32>().value(0);
    let constraints = constraints.as_struct();
    let kinds: Vec<_> = (0..constraints.len())
        .map(|index| {
            (
                text(&row(field(constraints, "name"), index)).unwrap(),
                text(&row(field(constraints, "kind"), index)).unwrap(),
                field(constraints, "enforced").as_boolean().value(index),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("orders_pkey".to_owned(), "primary_key".to_owned(), true),
            ("orders_reference_key".to_owned(), "unique".to_owned(), true),
            (
                "orders_customer_fkey".to_owned(),
                "foreign_key".to_owned(),
                false
            ),
            ("orders_amount_check".to_owned(), "check".to_owned(), true),
        ]
    );
    assert_eq!(
        path(&row(field(constraints, "columns"), 0)),
        Some(strings(&["id"]))
    );
    assert_eq!(
        text(&row(field(constraints, "unique_kind"), 1)).as_deref(),
        Some("index")
    );
    assert_eq!(
        text(&row(field(constraints, "condition"), 1)).as_deref(),
        Some("(amount > 0)")
    );
    assert_eq!(
        path(&row(field(constraints, "referenced_table"), 2)),
        Some(strings(&["test", "public", "customers"]))
    );
    assert_eq!(
        path(&row(field(constraints, "referenced_columns"), 2)),
        Some(strings(&["customer_id"]))
    );
    assert!(field(constraints, "columns").is_null(3));
    assert_eq!(
        text(&row(field(constraints, "expression"), 3)).as_deref(),
        Some("(amount >= 0)")
    );

    let source_metadata: TestTable =
        serde_json::from_str(&text(column(&orders, "source_metadata")).unwrap()).unwrap();
    assert_eq!(
        source_metadata,
        TestTable {
            table: strings(&["test", "public", "orders"]),
            fixture: TestFixture::Orders,
        }
    );

    let retrieval = column(&orders, "retrieval").as_struct();
    let connector = field(retrieval, "connector").as_struct();
    assert_eq!(text(field(connector, "name")).as_deref(), Some("test"));
    assert_eq!(text(field(connector, "version")).as_deref(), Some("0.1.0"));
    assert!(field(retrieval, "adbc_driver").is_null(0));
    assert_eq!(
        text(field(retrieval, "library_version")).as_deref(),
        Some(LIBRARY_VERSION)
    );
    assert_eq!(
        field(retrieval, "read_at")
            .as_primitive::<TimestampMicrosecondType>()
            .value(0),
        1_700_000_000_000_000
    );
}

#[test]
fn missing_and_never_offered_items_stay_distinct() {
    let (session, _) = session("missing-items");

    let mut all = rows(&session, tables(&["order_view", "events"]));
    let events = all.pop().unwrap();
    let view = all.pop().unwrap();

    assert_eq!(
        reported(column(&view, "source_id")),
        ("not_offered".to_owned(), None, None)
    );
    let view_constraints = column(&view, "constraints").as_struct();
    assert_eq!(
        text(field(view_constraints, "state")).as_deref(),
        Some("not_offered")
    );
    assert!(field(view_constraints, "value").is_null(0));

    assert_eq!(text(column(&events, "kind")).as_deref(), Some("other"));
    assert_eq!(
        text(column(&events, "source_kind")).as_deref(),
        Some("partitioned table")
    );
    assert_eq!(
        reported(column(&events, "source_id")),
        (
            "missing".to_owned(),
            None,
            Some("the table identifier is not readable".to_owned())
        )
    );
    assert!(
        column(&events, "columns_may_be_hidden")
            .as_boolean()
            .value(0)
    );
    let columns = column(&events, "columns").as_list::<i32>().value(0);
    let columns = columns.as_struct();
    assert_eq!(reported(field(columns, "source_id")).0, "missing");
    let types = field(columns, "type").as_struct();
    assert_eq!(text(field(types, "state")).as_deref(), Some("unread"));
    assert!(field(types, "data_source_type").is_null(0));
    assert_eq!(
        text(field(types, "unread_reason")).as_deref(),
        Some("the column type is not readable")
    );
    let constraints = column(&events, "constraints").as_struct();
    assert_eq!(
        text(field(constraints, "state")).as_deref(),
        Some("missing")
    );
    assert_eq!(
        text(field(constraints, "reason")).as_deref(),
        Some("the constraints are not readable")
    );
}

#[test]
fn a_failure_partway_ends_the_stream_after_complete_rows() {
    let (session, _) = session("partway");
    let operation = session
        .start_metadata(tables(&["orders", "fail", "events"]), None)
        .unwrap();

    let first = operation.pull().unwrap().unwrap();
    let error = operation.pull().err().unwrap();

    assert_eq!(text(column(&first, "state")).as_deref(), Some("complete"));
    assert_eq!(error.kind(), ErrorKind::DataSource);
    assert_eq!(operation.error().unwrap().kind(), ErrorKind::DataSource);
    assert!(operation.pull().unwrap().is_none());
    assert_eq!(rows(&session, tables(&["orders"])).len(), 1);
}

#[test]
fn cancelling_from_another_thread_stops_a_blocked_catalog_read() {
    let (session, probe) = session("cancel");
    let operation = session
        .start_metadata(
            MetadataRequest::NamespaceList {
                namespaces: filter(&["block"]),
            },
            None,
        )
        .unwrap();

    let pulled = thread::scope(|scope| {
        let puller = scope.spawn(|| operation.pull());
        wait_until(|| probe.sent.load(Ordering::SeqCst) == 1);
        operation.cancel().unwrap();
        puller.join().unwrap()
    });

    assert_eq!(pulled.err().unwrap().kind(), ErrorKind::Cancellation);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert!(session.start_metadata(tables(&["orders"]), None).is_ok());
}

#[test]
fn the_timeout_stops_a_blocked_catalog_read() {
    let (session, probe) = session("timeout");
    let operation = session
        .start_metadata(
            tables(&["orders", "block"]),
            Some(Duration::from_millis(50)),
        )
        .unwrap();

    assert!(operation.pull().unwrap().is_some());
    let error = operation.pull().err().unwrap();

    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
}

#[test]
fn releasing_partway_frees_the_session_and_a_busy_session_rejects_requests() {
    let (session, probe) = session("busy");
    let operation = session
        .start_metadata(
            MetadataRequest::TableList {
                namespaces: None,
                tables: None,
            },
            None,
        )
        .unwrap();
    assert!(operation.pull().unwrap().is_some());

    let busy = session.start_metadata(MetadataRequest::NamespaceList { namespaces: None }, None);
    assert_eq!(busy.err().unwrap().kind(), ErrorKind::InvalidState);
    assert!(
        operation.pull().unwrap().is_some(),
        "the running request continues"
    );

    operation.release();

    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert!(
        session
            .start_metadata(MetadataRequest::NamespaceList { namespaces: None }, None)
            .is_ok()
    );
}

#[test]
fn a_panic_in_a_catalog_read_becomes_internal_and_makes_the_session_unusable() {
    let (session, _) = session("panic");
    let operation = session.start_metadata(tables(&["panic"]), None).unwrap();

    assert_eq!(operation.pull().err().unwrap().kind(), ErrorKind::Internal);
    assert_eq!(
        session
            .start_metadata(tables(&["orders"]), None)
            .err()
            .unwrap()
            .kind(),
        ErrorKind::InvalidState
    );
}
