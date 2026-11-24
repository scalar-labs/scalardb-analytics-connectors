//! The table metadata the connector derives from the org's describe results.
//!
//! Salesforce adds system fields to every object, so the checks name the
//! fields the mock models rather than comparing whole tables.

use std::collections::BTreeSet;

use analytics_types::{AnalyticsType, DecimalType};
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, TableMetadata, TableRecord,
    UniqueKind, runtime::TableRead,
};

use super::{OBJECT, Org, reference};

pub fn check(org: &Org) {
    let object = table_metadata(org, OBJECT);
    for (name, nullable, data_source_type, analytics_type) in test_fields() {
        let column = column(&object, name);
        assert_eq!(
            (column.nullable, &column.ty),
            (nullable, &mapped(data_source_type, analytics_type)),
            "{OBJECT}.{name}"
        );
    }
    let Reported::Value(constraints) = object.constraints() else {
        panic!("{OBJECT}: constraints are not read");
    };
    for expected in [
        primary_key(),
        unique("Key__c"),
        foreign_key("Reference__c", "ConnectorTestBulk__c"),
    ] {
        assert!(
            constraints.contains(&expected),
            "{OBJECT}: no constraint {expected:?} among {constraints:?}"
        );
    }
    assert!(
        object.columns_may_be_hidden(),
        "{OBJECT}: field-level security can hide columns"
    );
    assert!(
        object
            .columns()
            .iter()
            .all(|column| column.name != "FlsHidden__c"),
        "{OBJECT}.FlsHidden__c, which field-level security hides, is a column"
    );

    // Standard fields have the types a custom field cannot.
    for (object, field, data_source_type, analytics_type) in [
        ("Task", "Subject", "combobox", AnalyticsType::Text),
        ("Task", "CallDurationInSeconds", "int", AnalyticsType::Int),
        // Every org has this read-only `long`; the others REST API version
        // 62.0 describes come with an org's own features.
        (
            "PlatformEventUsageMetric",
            "Value",
            "long",
            AnalyticsType::BigInt,
        ),
    ] {
        let table = table_metadata(org, object);
        let column = column(&table, field);
        assert_eq!(
            (column.nullable, &column.ty),
            (true, &mapped(data_source_type, analytics_type)),
            "{object}.{field}"
        );
    }

    an_object_describe_does_not_find_is_not_found(org);
}

/// Salesforce answers describe for an object the user may not see as it does
/// for one that does not exist, so a name no org holds stands for both.
fn an_object_describe_does_not_find_is_not_found(org: &Org) {
    const MISSING: &str = "ConnectorTestMissing__c";
    let session = org.session();
    let listed = session
        .table_list(
            &None,
            &Some(BTreeSet::from([MISSING.to_owned()])),
            None,
            None,
        )
        .unwrap_or_else(|error| panic!("the table list fails: {error}"));
    assert!(listed.is_empty(), "{MISSING} is listed: {listed:?}");
    let result = session
        .table_metadata(&[reference(MISSING)], None, None)
        .unwrap_or_else(|error| panic!("{MISSING}: table metadata fails: {error}"));
    session.close();
    assert!(
        matches!(result.records.as_slice(), [TableRecord::NotFound { .. }]),
        "{MISSING}: {:?}",
        result.records
    );
}

/// The fields of `ConnectorTest__c` as name, nullability, Data Source type,
/// and Analytics type, matching `Values__c` of the mock where Salesforce can
/// create the field.
fn test_fields() -> Vec<(&'static str, bool, &'static str, AnalyticsType)> {
    use AnalyticsType::*;
    vec![
        ("Id", false, "id", Text),
        ("Name", true, "string", Text),
        ("Key__c", true, "string", Text),
        ("LongTextArea__c", true, "textarea", Text),
        ("Picklist__c", true, "picklist", Text),
        ("MultiPicklist__c", true, "multipicklist", Text),
        ("Email__c", true, "email", Text),
        ("Phone__c", true, "phone", Text),
        ("Url__c", true, "url", Text),
        ("Encryptedstring__c", true, "encryptedstring", Text),
        ("Reference__c", true, "reference", Text),
        ("Checkbox__c", false, "boolean", Boolean),
        ("Number18_2__c", true, "double(18,2)", decimal(18, 2)),
        ("Percent5_2__c", true, "percent(5,2)", decimal(5, 2)),
        ("Currency18_2__c", true, "currency(18,2)", decimal(18, 2)),
        ("Date__c", true, "date", Date),
        ("DateTime__c", true, "datetime", TimestampTz),
        ("Time__c", true, "time", Time),
    ]
}

/// Reads one object's table metadata.
fn table_metadata(org: &Org, object: &str) -> TableMetadata {
    let session = org.session();
    let result = session
        .table_metadata(&[reference(object)], None, None)
        .unwrap_or_else(|error| panic!("{object}: table metadata fails: {error}"));
    session.close();
    match result.records.into_iter().next() {
        Some(TableRecord::Read {
            metadata: TableRead { table_metadata, .. },
            ..
        }) => table_metadata,
        other => panic!("{object}: not read: {other:?}"),
    }
}

fn column<'a>(table: &'a TableMetadata, name: &str) -> &'a Column {
    table
        .columns()
        .iter()
        .find(|column| column.name == name)
        .unwrap_or_else(|| panic!("{name}: no such column"))
}

fn mapped(data_source_type: &str, analytics_type: AnalyticsType) -> ColumnType {
    ColumnType::Mapped {
        data_source_type: data_source_type.to_owned(),
        analytics_type,
    }
}

fn decimal(precision: u8, scale: i8) -> AnalyticsType {
    AnalyticsType::Decimal(DecimalType::new(precision, scale).unwrap())
}

fn primary_key() -> Constraint {
    Constraint {
        name: "Id".to_owned(),
        enforced: true,
        kind: ConstraintKind::PrimaryKey {
            columns: vec!["Id".to_owned()],
        },
    }
}

fn unique(field: &str) -> Constraint {
    Constraint {
        name: field.to_owned(),
        enforced: true,
        kind: ConstraintKind::Unique {
            columns: vec![field.to_owned()],
            kind: UniqueKind::Constraint,
            condition: None,
        },
    }
}

fn foreign_key(field: &str, target: &str) -> Constraint {
    Constraint {
        name: field.to_owned(),
        enforced: true,
        kind: ConstraintKind::ForeignKey {
            columns: vec![field.to_owned()],
            referenced_table: reference(target),
            referenced_columns: vec!["Id".to_owned()],
        },
    }
}
