//! Metadata retrieval behavior specific to the PostgreSQL connector.
//!
//! The conformance suite (`tests/conformance`) checks the table metadata the
//! contract defines. These tests check what only this connector states: its
//! source metadata document, the driver it reports, and the effect of its
//! `search_path` option.

mod support;

use connector_postgresql::{
    PostgresCheck, PostgresConnector, PostgresDomainCheck, PostgresForeignKey,
    PostgresGeneratedStorage, PostgresIdentity, PostgresKey, PostgresNotNull, PostgresPrimaryKey,
    PostgresReferencedTable, PostgresTable, PostgresTableKind, PostgresType, PostgresTypeForm,
    PostgresUniqueKey, PostgresUniqueKind, PostgresValueSource,
};
use connector_sdk::{
    Catalog, Component, Connector, SecretString, TableMetadata, TableRecord, TableReference,
    start_source_read,
};

use support::{PASSWORD, collect_ok, library, open, open_session, options, server, session};

type PostgresCatalog = <PostgresConnector as Connector>::Catalog;

const SCHEMA: &str = "metadata_test";

fn setup() {
    server().reset_schema(
        SCHEMA,
        r#"
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
COMMENT ON TYPE mood IS 'How one feels';
CREATE TABLE parent (
  id bigint PRIMARY KEY,
  code text NOT NULL CONSTRAINT parent_code_key UNIQUE,
  feeling mood,
  moods mood[]
);
CREATE UNIQUE INDEX lower_code ON parent (lower(code));
COMMENT ON TABLE parent IS 'Parents of the fixture';
COMMENT ON COLUMN parent.code IS 'Business code';
CREATE TABLE child (
  id bigint PRIMARY KEY,
  parent_id bigint CONSTRAINT child_parent_fkey REFERENCES parent (id),
  starts_on date,
  ends_on date,
  CONSTRAINT valid_period CHECK (starts_on <= ends_on)
);
CREATE DOMAIN positive AS integer CHECK (VALUE > 0);
CREATE DOMAIN small_positive AS positive NOT NULL CONSTRAINT below_hundred CHECK (VALUE < 100);
COMMENT ON DOMAIN small_positive IS 'A positive number below 100';
CREATE TABLE filled (
  id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  parent_id bigint CONSTRAINT filled_parent_fkey REFERENCES parent (id),
  quantity small_positive,
  amount integer CONSTRAINT amount_non_negative CHECK (amount >= 0),
  doubled integer GENERATED ALWAYS AS (amount * 2) STORED,
  created_at timestamptz DEFAULT now(),
  CONSTRAINT filled_parent_created_key UNIQUE (parent_id, created_at)
);
COMMENT ON TABLE filled IS 'Order lines';
COMMENT ON COLUMN filled.quantity IS 'Units ordered';
CREATE VIEW parent_codes AS SELECT id, code FROM parent;
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

fn nullable(table_metadata: &TableMetadata, name: &str) -> Option<bool> {
    table_metadata
        .columns()
        .iter()
        .find(|column| column.name == name)
        .map(|column| column.nullable)
}

#[test]
fn metadata_retrieval_states_what_only_postgresql_has() {
    setup();
    source_metadata_restores_from_its_document_only_when_its_types_resolve();
    source_metadata_states_comments_and_view_definitions();
    source_metadata_states_keys_and_checks_by_concept();
    source_metadata_lists_the_types_columns_lead_to();
    source_metadata_states_value_sources_and_not_null_by_path();
    sessions_report_the_driver_from_its_manifest();
    the_configured_search_path_spells_data_source_types();
}

/// The document restores to the same value, which is how the SDK restores a
/// stored document for comparison. One whose types do not resolve, because
/// it refers to a type it does not list, its domains form a cycle, or it
/// lists a type twice, cannot be restored, so comparing it is an invalid
/// request.
fn source_metadata_restores_from_its_document_only_when_its_types_resolve() {
    let source = read_one(&session(), "parent");
    let document = serde_json::to_string(&source).unwrap();
    assert_eq!(
        serde_json::from_str::<PostgresTable>(&document).unwrap(),
        source
    );
    let restores = |document: &serde_json::Value| {
        serde_json::from_value::<PostgresTable>(document.clone()).is_ok()
    };

    let mut unlisted: serde_json::Value = serde_json::from_str(&document).unwrap();
    unlisted["types"]
        .as_array_mut()
        .unwrap()
        .retain(|ty| ty["name"] != "metadata_test.mood");
    assert!(!restores(&unlisted));

    let filled = serde_json::to_value(read_one(&session(), "filled")).unwrap();
    assert!(restores(&filled));
    let mut cycle = filled.clone();
    for ty in cycle["types"].as_array_mut().unwrap() {
        if ty["name"] == "metadata_test.positive" {
            ty["base"] = "metadata_test.small_positive".into();
        }
    }
    assert!(!restores(&cycle));

    let mut duplicated = filled;
    let first = duplicated["types"][0].clone();
    duplicated["types"].as_array_mut().unwrap().push(first);
    assert!(!restores(&duplicated));
}

/// Comments on tables, columns, and types, and a view's query, reach the
/// source metadata; a table without them states none, and built-in types'
/// own descriptions are not read.
fn source_metadata_states_comments_and_view_definitions() {
    let parent = read_one(&session(), "parent");
    assert_eq!(parent.kind, PostgresTableKind::Table);
    assert_eq!(parent.comment.as_deref(), Some("Parents of the fixture"));
    assert_eq!(parent.definition, None);
    let column_comment = |name: &str| {
        parent
            .columns
            .iter()
            .find(|column| column.name == name)
            .map(|column| column.comment.clone())
    };
    assert_eq!(
        column_comment("code"),
        Some(Some("Business code".to_owned()))
    );
    assert_eq!(column_comment("feeling"), Some(None));
    let type_comment = |name: &str| {
        parent
            .types
            .iter()
            .find(|ty| ty.name == name)
            .map(|ty| ty.comment.clone())
    };
    assert_eq!(
        type_comment("metadata_test.mood"),
        Some(Some("How one feels".to_owned()))
    );
    assert_eq!(type_comment("text"), Some(None));

    let view = read_one(&session(), "parent_codes");
    assert_eq!(view.kind, PostgresTableKind::View);
    assert_eq!(view.comment, None);
    let definition = view.definition.unwrap();
    assert!(definition.contains("SELECT") && definition.contains("parent"));
}

/// Primary keys, unique keys, including a unique index over an expression,
/// foreign keys, and CHECK constraints are stated each in their own list.
fn source_metadata_states_keys_and_checks_by_concept() {
    let parent = read_one(&session(), "parent");
    assert_eq!(
        parent.primary_key,
        Some(PostgresPrimaryKey {
            name: "parent_pkey".to_owned(),
            columns: vec!["id".to_owned()],
        })
    );
    assert_eq!(
        parent.unique_keys,
        vec![
            PostgresUniqueKey {
                name: "lower_code".to_owned(),
                kind: PostgresUniqueKind::Index,
                keys: vec![PostgresKey::Expression("lower(code)".to_owned())],
                condition: None,
            },
            PostgresUniqueKey {
                name: "parent_code_key".to_owned(),
                kind: PostgresUniqueKind::Constraint,
                keys: vec![PostgresKey::Column("code".to_owned())],
                condition: None,
            },
        ]
    );

    let child = read_one(&session(), "child");
    assert_eq!(
        child.foreign_keys,
        vec![PostgresForeignKey {
            name: "child_parent_fkey".to_owned(),
            columns: vec!["parent_id".to_owned()],
            referenced: PostgresReferencedTable {
                namespace: SCHEMA.to_owned(),
                table: "parent".to_owned(),
                columns: vec!["id".to_owned()],
            },
            enforced: true,
            validated: true,
        }]
    );
    assert_eq!(
        child.checks,
        vec![PostgresCheck {
            name: "valid_period".to_owned(),
            expression: "(starts_on <= ends_on)".to_owned(),
            enforced: true,
            validated: true,
        }]
    );
}

/// The table lists each type its columns lead to once, by display name:
/// domains with their base and CHECK constraints, and arrays with their
/// element type.
fn source_metadata_lists_the_types_columns_lead_to() {
    let base = |name: &str, typname: &str| PostgresType {
        name: name.to_owned(),
        schema: "pg_catalog".to_owned(),
        typname: typname.to_owned(),
        comment: None,
        form: PostgresTypeForm::Base,
    };
    let filled = read_one(&session(), "filled");
    assert_eq!(
        filled.types,
        vec![
            base("bigint", "int8"),
            base("integer", "int4"),
            PostgresType {
                name: "metadata_test.positive".to_owned(),
                schema: SCHEMA.to_owned(),
                typname: "positive".to_owned(),
                comment: None,
                form: PostgresTypeForm::Domain {
                    base: "integer".to_owned(),
                    typmod: -1,
                    not_null: false,
                    checks: vec![PostgresDomainCheck {
                        name: "positive_check".to_owned(),
                        expression: "(VALUE > 0)".to_owned(),
                        validated: true,
                    }],
                },
            },
            PostgresType {
                name: "metadata_test.small_positive".to_owned(),
                schema: SCHEMA.to_owned(),
                typname: "small_positive".to_owned(),
                comment: Some("A positive number below 100".to_owned()),
                form: PostgresTypeForm::Domain {
                    base: "metadata_test.positive".to_owned(),
                    typmod: -1,
                    not_null: true,
                    checks: vec![PostgresDomainCheck {
                        name: "below_hundred".to_owned(),
                        expression: "((VALUE)::integer < 100)".to_owned(),
                        validated: true,
                    }],
                },
            },
            base("timestamp with time zone", "timestamptz"),
        ]
    );

    let parent = read_one(&session(), "parent");
    let moods = parent
        .types
        .iter()
        .find(|ty| ty.name == "metadata_test.mood[]")
        .unwrap();
    assert_eq!(
        moods.form,
        PostgresTypeForm::Array {
            element: "metadata_test.mood".to_owned()
        }
    );
    assert!(parent.types.iter().any(|ty| ty.name == "metadata_test.mood"
        && matches!(&ty.form, PostgresTypeForm::Enum { labels } if labels.len() == 3)));
}

/// Columns state where PostgreSQL takes their values from other than the
/// row, and their own NOT NULL apart from their types'; table metadata
/// combines the validated ones.
fn source_metadata_states_value_sources_and_not_null_by_path() {
    let filled = read_one(&session(), "filled");
    let column = |name: &str| {
        filled
            .columns
            .iter()
            .find(|column| column.name == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        column("id").value_source,
        Some(PostgresValueSource::Identity {
            generation: PostgresIdentity::Always
        })
    );
    assert_eq!(
        column("doubled").value_source,
        Some(PostgresValueSource::Generated {
            storage: PostgresGeneratedStorage::Stored,
            expression: "(amount * 2)".to_owned(),
        })
    );
    assert_eq!(
        column("created_at").value_source,
        Some(PostgresValueSource::Default {
            expression: "now()".to_owned()
        })
    );
    assert_eq!(column("amount").value_source, None);

    let quantity = column("quantity");
    assert_eq!(quantity.ty, "metadata_test.small_positive");
    // The NOT NULL is the domain's, not the column's own.
    assert_eq!(quantity.not_null, None);
    assert_eq!(
        column("id").not_null,
        Some(PostgresNotNull { validated: true })
    );
    let table_metadata = filled.to_table_metadata();
    assert_eq!(nullable(&table_metadata, "id"), Some(false));
    assert_eq!(nullable(&table_metadata, "quantity"), Some(false));
    assert_eq!(nullable(&table_metadata, "amount"), Some(true));

    // PostgreSQL 18 allows a NOT NULL that existing rows may violate.
    let version: i32 = server()
        .psql("SHOW server_version_num")
        .trim()
        .parse()
        .unwrap();
    if version >= 180_000 {
        server().psql(&format!(
            "INSERT INTO {SCHEMA}.filled (quantity, amount) VALUES (1, NULL); \
             ALTER TABLE {SCHEMA}.filled ADD CONSTRAINT amount_not_null NOT NULL amount NOT VALID;"
        ));
        let filled = read_one(&session(), "filled");
        let amount = filled
            .columns
            .iter()
            .find(|column| column.name == "amount")
            .unwrap();
        assert_eq!(amount.not_null, Some(PostgresNotNull { validated: false }));
        assert_eq!(nullable(&filled.to_table_metadata(), "amount"), Some(true));
    }
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
