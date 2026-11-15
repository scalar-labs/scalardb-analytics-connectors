//! The metadata requests and the table metadata the contract expects of the
//! fixture's objects.

use std::collections::BTreeSet;

use analytics_types::{AnalyticsType, DecimalType};
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Listed, NamespacePath, Reported, TableEntry,
    TableKind, TableMetadata, TableReference, UniqueKind, UnsupportedReason,
    conformance::{ExpectedRecord, MetadataCase, MetadataRequest},
};

use super::{NAMESPACE, OUTSIDER, SalesforceFixture, objects, reference};

pub fn cases(fixture: &SalesforceFixture) -> Vec<MetadataCase> {
    let reader = || fixture.options(super::READER);
    let names = |names: &[&str]| {
        Some(
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<BTreeSet<_>>(),
        )
    };
    let every_object: Vec<&str> = objects().keys().copied().collect();
    vec![
        MetadataCase {
            name: "the one namespace is listed",
            options: reader(),
            request: MetadataRequest::Namespaces {
                filter: None,
                expected: vec![namespace()],
            },
        },
        MetadataCase {
            name: "a namespace filter matches exactly",
            options: reader(),
            request: MetadataRequest::Namespaces {
                filter: names(&[NAMESPACE, "SObjects", "other"]),
                expected: vec![namespace()],
            },
        },
        MetadataCase {
            name: "a namespace filter that differs in case matches nothing",
            options: reader(),
            request: MetadataRequest::Namespaces {
                filter: names(&["SObjects"]),
                expected: Vec::new(),
            },
        },
        MetadataCase {
            name: "every visible object is listed by name",
            options: reader(),
            request: MetadataRequest::Tables {
                namespaces: None,
                tables: None,
                expected: tables(&every_object),
            },
        },
        MetadataCase {
            name: "a table filter matches exactly, without case folding",
            options: reader(),
            request: MetadataRequest::Tables {
                namespaces: names(&[NAMESPACE]),
                tables: names(&["Contact", "contact", "Missing__c"]),
                expected: tables(&["Contact"]),
            },
        },
        MetadataCase {
            name: "another namespace lists no tables",
            options: reader(),
            request: MetadataRequest::Tables {
                namespaces: names(&["other"]),
                tables: None,
                expected: Vec::new(),
            },
        },
        MetadataCase {
            name: "objects the user may not see are not listed",
            options: fixture.options(OUTSIDER),
            request: MetadataRequest::Tables {
                namespaces: None,
                tables: None,
                expected: tables(&["Account", "Contact", "Locked__c", "Values__c"]),
            },
        },
        MetadataCase {
            name: "every mapping rule, unmapped types, and constraints",
            options: reader(),
            request: MetadataRequest::TableMetadata {
                tables: vec![
                    reference("Values__c"),
                    reference("Unmapped__c"),
                    reference("Contact"),
                    reference("Account"),
                ],
                expected: vec![
                    ExpectedRecord::Read(values()),
                    ExpectedRecord::Read(unmapped()),
                    ExpectedRecord::Read(contact(true)),
                    ExpectedRecord::Read(account()),
                ],
            },
        },
        MetadataCase {
            name: "names that do not describe as exactly one object",
            options: reader(),
            request: MetadataRequest::TableMetadata {
                tables: vec![
                    reference("Missing__c"),
                    reference("contact"),
                    TableReference::new(vec!["other".to_owned(), "Contact".to_owned()]).unwrap(),
                    TableReference::new(vec!["Contact".to_owned()]).unwrap(),
                ],
                // Salesforce hides objects the user may not see, so a
                // missing object in the namespace could exist. Object names
                // are unique without regard to case, so `contact` cannot
                // exist beside `Contact`, and a name outside the namespace
                // cannot exist at all.
                expected: vec![
                    ExpectedRecord::NotFound,
                    ExpectedRecord::DoesNotExist,
                    ExpectedRecord::DoesNotExist,
                    ExpectedRecord::DoesNotExist,
                ],
            },
        },
        MetadataCase {
            name: "objects and fields the user may not see",
            options: fixture.options(OUTSIDER),
            request: MetadataRequest::TableMetadata {
                tables: vec![
                    reference("Restricted__c"),
                    reference("Locked__c"),
                    reference("Contact"),
                ],
                expected: vec![
                    ExpectedRecord::NotFound,
                    ExpectedRecord::PermissionDenied,
                    ExpectedRecord::Read(contact(false)),
                ],
            },
        },
    ]
}

fn namespace() -> Listed<NamespacePath> {
    Listed::Item(NamespacePath {
        names: vec![NAMESPACE.to_owned()],
    })
}

fn tables(names: &[&str]) -> Vec<Listed<TableEntry>> {
    names
        .iter()
        .map(|name| {
            Listed::Item(TableEntry {
                table: reference(name),
                kind: TableKind::Table,
            })
        })
        .collect()
}

fn mapped(data_source_type: &str, analytics_type: AnalyticsType) -> ColumnType {
    ColumnType::Mapped {
        data_source_type: data_source_type.to_owned(),
        analytics_type,
    }
}

fn unsupported(data_source_type: &str, reason: UnsupportedReason) -> ColumnType {
    ColumnType::Unsupported {
        data_source_type: data_source_type.to_owned(),
        reason,
    }
}

fn decimal(precision: u8, scale: i8) -> AnalyticsType {
    AnalyticsType::Decimal(DecimalType::new(precision, scale).unwrap())
}

/// Table metadata of an object with `columns` as name, nullability, and type.
fn table(
    name: &str,
    columns: Vec<(&str, bool, ColumnType)>,
    constraints: Vec<Constraint>,
) -> TableMetadata {
    TableMetadata::new(
        reference(name),
        TableKind::Table,
        Reported::NotOffered,
        columns
            .into_iter()
            .zip(1..)
            .map(|((name, nullable, ty), ordinal)| Column {
                name: name.to_owned(),
                ordinal,
                nullable,
                source_id: Reported::NotOffered,
                ty,
            })
            .collect(),
        true,
        Reported::Value(constraints),
    )
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

pub fn values() -> TableMetadata {
    use AnalyticsType::*;
    table(
        "Values__c",
        vec![
            ("Id", false, mapped("id", Text)),
            ("Name", true, mapped("string", Text)),
            ("Notes__c", true, mapped("textarea", Text)),
            ("Stage__c", true, mapped("picklist", Text)),
            ("Tags__c", true, mapped("multipicklist", Text)),
            ("Kind__c", true, mapped("combobox", Text)),
            ("Email__c", true, mapped("email", Text)),
            ("Phone__c", true, mapped("phone", Text)),
            ("Site__c", true, mapped("url", Text)),
            ("Code__c", true, mapped("encryptedstring", Text)),
            ("Owner__c", true, mapped("reference", Text)),
            ("Flag__c", false, mapped("boolean", Boolean)),
            ("Count__c", true, mapped("int", Int)),
            ("Big__c", true, mapped("long", BigInt)),
            ("Amount__c", true, mapped("double(18,2)", decimal(18, 2))),
            ("Rate__c", true, mapped("percent(5,2)", decimal(5, 2))),
            ("Price__c", true, mapped("currency(18,2)", decimal(18, 2))),
            ("Day__c", true, mapped("date", Date)),
            ("Moment__c", true, mapped("datetime", TimestampTz)),
            ("Clock__c", true, mapped("time", Time)),
        ],
        vec![primary_key(), foreign_key("Owner__c", "Account")],
    )
}

fn unmapped() -> TableMetadata {
    use UnsupportedReason::{MappingNotImplemented, NotMapped, ParameterOutOfRange};
    table(
        "Unmapped__c",
        vec![
            ("Id", false, mapped("id", AnalyticsType::Text)),
            ("File__c", true, unsupported("base64", NotMapped)),
            ("BillingAddress", true, unsupported("address", NotMapped)),
            ("Spot__c", true, unsupported("location", NotMapped)),
            ("Value__c", true, unsupported("anyType", NotMapped)),
            ("Setting__c", true, unsupported("complexvalue", NotMapped)),
            (
                "Unscaled__c",
                true,
                unsupported("double(0,0)", ParameterOutOfRange),
            ),
            (
                "Huge__c",
                true,
                unsupported("double(40,2)", MappingNotImplemented),
            ),
        ],
        vec![primary_key()],
    )
}

/// `Contact` as the reader sees it, or without the field the outsider may
/// not see.
fn contact(with_email: bool) -> TableMetadata {
    let mut columns = vec![
        ("Id", false, mapped("id", AnalyticsType::Text)),
        ("LastName", false, mapped("string", AnalyticsType::Text)),
    ];
    let mut constraints = vec![primary_key(), foreign_key("AccountId", "Account")];
    if with_email {
        columns.push(("Email", true, mapped("email", AnalyticsType::Text)));
        constraints.push(Constraint {
            name: "Email".to_owned(),
            enforced: true,
            kind: ConstraintKind::Unique {
                columns: vec!["Email".to_owned()],
                kind: UniqueKind::Constraint,
                condition: None,
            },
        });
    }
    columns.push(("AccountId", true, mapped("reference", AnalyticsType::Text)));
    columns.push(("WhoId", true, mapped("reference", AnalyticsType::Text)));
    table("Contact", columns, constraints)
}

fn account() -> TableMetadata {
    table(
        "Account",
        vec![
            ("Id", false, mapped("id", AnalyticsType::Text)),
            ("Name", false, mapped("string", AnalyticsType::Text)),
        ],
        vec![primary_key()],
    )
}
