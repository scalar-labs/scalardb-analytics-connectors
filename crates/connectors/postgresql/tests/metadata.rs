//! Metadata retrieval behavior specific to the PostgreSQL connector.
//!
//! The conformance suite (`tests/conformance`) checks the table metadata the
//! contract defines. These tests check what only this connector states: its
//! source metadata document, the driver it reports, and the effect of its
//! `search_path` option.

mod support;

use connector_postgresql::{PostgresConnector, PostgresTable};
use connector_sdk::{
    Catalog, Component, Connector, SecretString, TableRecord, TableReference, start_source_read,
};

use support::{PASSWORD, collect_ok, library, open, open_session, options, server, session};

type PostgresCatalog = <PostgresConnector as Connector>::Catalog;

const SCHEMA: &str = "metadata_test";

fn setup() {
    server().reset_schema(
        SCHEMA,
        r#"
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
CREATE TABLE parent (id bigint PRIMARY KEY, code text NOT NULL, feeling mood);
CREATE UNIQUE INDEX lower_code ON parent (lower(code));
"#,
    );
}

fn reference(name: &str) -> TableReference {
    TableReference::new(vec![SCHEMA.to_owned(), name.to_owned()]).unwrap()
}

fn read_one(session: &support::PostgresSession, name: &str) -> PostgresTable {
    let mut records = start_source_read(None, || {
        PostgresCatalog::table_metadata(session, &[reference(name)])
    })
    .unwrap();
    match collect_ok(&mut records).remove(0) {
        TableRecord::Read { metadata, .. } => metadata,
        other => panic!("{name} was not read: {other:?}"),
    }
}

#[test]
fn metadata_retrieval_states_what_only_postgresql_has() {
    setup();
    source_metadata_keeps_expression_indexes_and_restores_from_its_document();
    sessions_report_the_driver_from_its_manifest();
    the_configured_search_path_spells_data_source_types();
}

/// An expression index is no constraint in table metadata, but the source
/// metadata keeps it, and the document restores to the same value.
fn source_metadata_keeps_expression_indexes_and_restores_from_its_document() {
    let source = read_one(&session(), "parent");
    assert!(
        source
            .unique_indexes
            .iter()
            .any(|index| index.name == "lower_code" && index.has_expressions)
    );
    let document = serde_json::to_string(&source).unwrap();
    assert_eq!(
        serde_json::from_str::<PostgresTable>(&document).unwrap(),
        source
    );
}

fn sessions_report_the_driver_from_its_manifest() {
    assert_eq!(
        Some(session().access().driver().clone()),
        Some(Component {
            name: "ADBC PostgreSQL Driver".to_owned(),
            version: "1.13.0".to_owned(),
        })
    );
}

/// Without the option the server's path applies, so types outside it are
/// qualified; with it, types on the configured path are not.
fn the_configured_search_path_spells_data_source_types() {
    let feeling = |metadata: &PostgresTable| {
        metadata
            .columns
            .iter()
            .find(|column| column.name == "feeling")
            .map(|column| column.data_source_type.clone())
    };
    assert_eq!(
        feeling(&read_one(&session(), "parent")).as_deref(),
        Some("metadata_test.mood")
    );

    let mut configured = options("postgres", PASSWORD);
    configured.push((
        "search_path".to_owned(),
        SecretString::new(format!("{SCHEMA}, public")),
    ));
    let session = open(&library(), configured)
        .and_then(|data_source| open_session(&data_source))
        .unwrap();
    assert_eq!(
        feeling(&read_one(&session, "parent")).as_deref(),
        Some("mood")
    );
}
