//! Metadata requests driven through the runtime with the in-memory test
//! Connector: their results, and the calling conventions they share with reads.

use std::{
    collections::BTreeSet,
    sync::{Arc, atomic::Ordering},
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use connector_sdk::{
    Component, ErrorKind, LibrarySettings, Listed, NameFilter, NamespacePath, SecretString,
    SourceTableMetadata, TableEntry, TableKind, TableRecord, TableReference,
    runtime::{
        BuiltInConnector, Cancellation, Library, Retrieval, SessionState, SourceMetadata, TableRead,
    },
    test_connector::{Probe, TestConnector, TestFixture, TestTable, probe},
};

const LIBRARY_VERSION: &str = "0.0.0-test";

fn library() -> Arc<Library> {
    Library::initialize(
        &[BuiltInConnector::of::<TestConnector>()],
        &LibrarySettings::default(),
        LIBRARY_VERSION,
    )
    .unwrap()
}

fn session(label: &str) -> (Arc<SessionState>, Arc<Probe>) {
    let label = format!("metadata-{label}");
    let options = vec![("label".to_owned(), SecretString::new(label.clone()))];
    let data_source = library().open_data_source("test", options, None).unwrap();
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

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn table(name: &str) -> TableReference {
    TableReference::new(names(&["test", "public", name])).unwrap()
}

fn tables(names: &[&str]) -> Vec<TableReference> {
    names.iter().map(|name| table(name)).collect()
}

fn records(session: &Arc<SessionState>, names: &[&str]) -> Vec<TableRecord<TableRead>> {
    session
        .table_metadata(&tables(names), None, None)
        .unwrap()
        .records
}

fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached in time");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_namespace_list_returns_listed_and_unreadable_ranges_matching_names_exactly() {
    let (session, _) = session("namespaces");

    let all = session.namespace_list(&None, None, None).unwrap();

    assert_eq!(
        all,
        vec![
            Listed::Item(NamespacePath {
                names: names(&["test", "public"]),
            }),
            Listed::Item(NamespacePath {
                names: names(&["test", "sales"]),
            }),
            Listed::Unreadable {
                scope: NamespacePath {
                    names: names(&["test", "restricted"]),
                },
                reason: "permission denied for the namespace".to_owned(),
            },
        ]
    );
    let sales = session
        .namespace_list(&filter(&["sales"]), None, None)
        .unwrap();
    assert_eq!(sales.len(), 1);
    let none = session
        .namespace_list(&filter(&["Sales"]), None, None)
        .unwrap();
    assert!(none.is_empty(), "names match without case folding");
}

#[test]
fn a_table_list_returns_each_table_kind_and_unreadable_ranges() {
    let (session, _) = session("tables");

    let all = session.table_list(&None, &None, None, None).unwrap();

    let entry = |path: &[&str], kind| {
        Listed::Item(TableEntry {
            table: TableReference::new(names(path)).unwrap(),
            kind,
        })
    };
    assert_eq!(
        all,
        vec![
            entry(&["test", "public", "orders"], TableKind::Table),
            entry(&["test", "public", "order_view"], TableKind::View),
            entry(
                &["test", "sales", "events"],
                TableKind::Other("partitioned table".to_owned()),
            ),
            Listed::Unreadable {
                scope: NamespacePath {
                    names: names(&["test", "restricted"]),
                },
                reason: "permission denied for the namespace".to_owned(),
            },
        ]
    );
    let orders = session
        .table_list(&filter(&["public"]), &filter(&["orders"]), None, None)
        .unwrap();
    assert_eq!(orders.len(), 1);
}

#[test]
fn table_metadata_returns_one_record_per_table_in_request_order() {
    let (session, _) = session("states");

    let result = session
        .table_metadata(
            &tables(&["orders", "events", "missing", "secret"]),
            None,
            None,
        )
        .unwrap();

    let read = |fixture| {
        let source = TestTable {
            table: names(&["test", "public", "orders"]),
            fixture,
        };
        TableRecord::Read {
            metadata: TableRead {
                table_metadata: source.to_table_metadata(),
                source_metadata: SourceMetadata {
                    connector: "test".to_owned(),
                    document: serde_json::to_string(&source).unwrap(),
                },
            },
            read_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        }
    };
    let TableRecord::Read {
        metadata: events, ..
    } = &result.records[1]
    else {
        panic!("events was read");
    };
    assert!(!events.table_metadata.is_complete());
    assert_eq!(result.records[0], read(TestFixture::Orders));
    assert_eq!(
        result.records[2..],
        [
            TableRecord::NotFound {
                table: table("missing"),
            },
            TableRecord::Unavailable {
                table: table("secret"),
                reason: "permission denied for the table".to_owned(),
            },
        ]
    );
    assert_eq!(
        result.retrieval,
        Retrieval {
            connector: Component {
                name: "test".to_owned(),
                version: "0.1.0".to_owned(),
            },
            adbc_driver: None,
            library_version: LIBRARY_VERSION.to_owned(),
        }
    );
}

#[test]
fn a_failure_returns_only_the_error_and_the_session_accepts_the_next_request() {
    let (session, _) = session("failure");

    let error = session
        .table_metadata(&tables(&["orders", "fail", "events"]), None, None)
        .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::DataSource);
    assert_eq!(records(&session, &["orders"]).len(), 1);
}

#[test]
fn cancelling_from_another_thread_stops_a_blocked_catalog_read() {
    let (session, probe) = session("cancel");
    let cancellation = Cancellation::new();

    let result = thread::scope(|scope| {
        let reader =
            scope.spawn(|| session.namespace_list(&filter(&["block"]), None, Some(&cancellation)));
        wait_until(|| probe.sent.load(Ordering::SeqCst) == 1);
        cancellation.cancel().unwrap();
        reader.join().unwrap()
    });

    assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancellation);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(probe.released.load(Ordering::SeqCst), 1);
    assert_eq!(records(&session, &["orders"]).len(), 1);
}

#[test]
fn a_cancellation_given_before_a_request_sends_nothing() {
    let (session, probe) = session("cancelled-before");
    let cancellation = Cancellation::new();
    cancellation.cancel().unwrap();

    for _ in 0..2 {
        let error = session
            .table_metadata(&tables(&["orders"]), None, Some(&cancellation))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Cancellation);
    }

    assert_eq!(probe.sent.load(Ordering::SeqCst), 0);
}

#[test]
fn the_timeout_stops_a_blocked_catalog_read() {
    let (session, probe) = session("timeout");

    let error = session
        .table_metadata(
            &tables(&["orders", "block"]),
            Some(Duration::from_millis(50)),
            None,
        )
        .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert_eq!(probe.cancelled.load(Ordering::SeqCst), 1);
}

#[test]
fn a_panic_in_a_catalog_read_becomes_internal_and_makes_the_session_unusable() {
    let (session, _) = session("panic");

    let error = session
        .table_metadata(&tables(&["panic"]), None, None)
        .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::Internal);
    assert_eq!(
        session
            .table_metadata(&tables(&["orders"]), None, None)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidState
    );
}
