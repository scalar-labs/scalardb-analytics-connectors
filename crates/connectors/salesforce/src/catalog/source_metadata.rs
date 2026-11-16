//! The source metadata document of one object, and the table metadata
//! derived from it.

use connector_salesforce_client::{FieldDescribe, SObjectDescribe};
use connector_sdk::{
    Column, Constraint, ConstraintKind, Reported, TableKind, TableMetadata, TableReference,
    UniqueKind,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::NAMESPACE;

/// What the connector read about one object: the attributes of its describe
/// result that state what the object and its fields mean.
///
/// It is the source metadata document. Fields are in describe order, which
/// is the object's column order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SalesforceObject {
    /// The object's API name, such as `Account` or `Invoice__c`.
    pub name: String,
    /// The object's display label in the Salesforce UI.
    pub label: String,
    /// Whether the object is a custom object.
    pub custom: bool,
    /// The object's fields in describe order.
    pub fields: Vec<SalesforceField>,
}

/// One field of a [`SalesforceObject`], as describe reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SalesforceField {
    /// The field's API name.
    pub name: String,
    /// The field's display label in the Salesforce UI.
    pub label: String,
    /// The describe `type`, such as `currency`.
    pub field_type: String,
    /// The SOAP type, such as `xsd:double`.
    pub soap_type: String,
    /// The maximum length of a text field, in characters.
    pub length: i64,
    /// The total number of digits of a numeric field.
    pub precision: i64,
    /// The number of digits after the decimal point of a numeric field.
    pub scale: i64,
    /// The maximum number of digits of an `int` field.
    pub digits: i64,
    /// Whether the field can be null.
    pub nillable: bool,
    /// Whether the field is a custom field.
    pub custom: bool,
    /// Whether the field is a formula field.
    pub calculated: bool,
    /// Whether comparisons of the field's values are case-sensitive.
    pub case_sensitive: bool,
    /// Whether the field can appear in a SOQL `WHERE` clause.
    pub filterable: bool,
    /// Whether Salesforce keeps the field's values unique.
    pub unique: bool,
    /// Whether the field is an external ID: an identifier of the record in
    /// another system.
    pub external_id: bool,
    /// The objects a reference field can point to.
    pub reference_to: Vec<String>,
    /// The name a reference field's relationship is traversed by in SOQL,
    /// such as `Account` for `AccountId`.
    pub relationship_name: Option<String>,
    /// The compound field this field is a component of, such as
    /// `BillingAddress`.
    pub compound_field_name: Option<String>,
}

/// The field every queryable object identifies its records by.
const ID_FIELD: &str = "Id";

impl SalesforceObject {
    pub(crate) fn from_describe(describe: SObjectDescribe) -> Self {
        Self {
            name: describe.name,
            label: describe.label,
            custom: describe.custom,
            fields: describe
                .fields
                .into_iter()
                .map(SalesforceField::from_describe)
                .collect(),
        }
    }

    pub fn table(&self) -> TableReference {
        reference(&self.name)
    }

    /// The field named exactly `name`.
    pub fn field(&self, name: &str) -> Option<&SalesforceField> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// The constraints Salesforce keeps: the record ID, unique fields, and
    /// lookups to one object.
    ///
    /// Salesforce does not name constraints, so each takes the name of its
    /// field. A polymorphic reference can point to several objects, so it is
    /// no foreign key; its targets stay in the source metadata.
    fn constraints(&self) -> Vec<Constraint> {
        let mut constraints = Vec::new();
        for field in &self.fields {
            let columns = vec![field.name.clone()];
            if field.name == ID_FIELD && field.field_type == "id" {
                constraints.push(Constraint {
                    name: field.name.clone(),
                    enforced: true,
                    kind: ConstraintKind::PrimaryKey { columns },
                });
                continue;
            }
            if field.unique {
                constraints.push(Constraint {
                    name: field.name.clone(),
                    enforced: true,
                    kind: ConstraintKind::Unique {
                        columns: columns.clone(),
                        kind: UniqueKind::Constraint,
                        condition: None,
                    },
                });
            }
            if let [target] = field.reference_to.as_slice() {
                constraints.push(Constraint {
                    name: field.name.clone(),
                    enforced: true,
                    kind: ConstraintKind::ForeignKey {
                        columns,
                        referenced_table: reference(target),
                        referenced_columns: vec![ID_FIELD.to_owned()],
                    },
                });
            }
        }
        constraints
    }
}

impl SalesforceField {
    fn from_describe(field: FieldDescribe) -> Self {
        Self {
            name: field.name,
            label: field.label,
            field_type: field.field_type,
            soap_type: field.soap_type,
            length: field.length,
            precision: field.precision,
            scale: field.scale,
            digits: field.digits,
            nillable: field.nillable,
            custom: field.custom,
            calculated: field.calculated,
            case_sensitive: field.case_sensitive,
            filterable: field.filterable,
            unique: field.unique,
            external_id: field.external_id,
            reference_to: field.reference_to,
            relationship_name: field.relationship_name,
            compound_field_name: field.compound_field_name,
        }
    }
}

impl SalesforceObject {
    /// Derives the table metadata. Field-level security hides fields from
    /// describe without saying so, so columns may always be hidden.
    pub fn table_metadata(&self) -> TableMetadata {
        let columns = self
            .fields
            .iter()
            .zip(1..)
            .map(|(field, ordinal)| Column {
                name: field.name.clone(),
                ordinal,
                nullable: field.nillable,
                source_id: Reported::NotOffered,
                ty: field.column_type(),
            })
            .collect();
        TableMetadata::new(
            self.table(),
            TableKind::Table,
            Reported::NotOffered,
            columns,
            true,
            Reported::Value(self.constraints()),
        )
    }
}

/// The table reference of the object named `name`.
pub(crate) fn reference(name: &str) -> TableReference {
    TableReference::new(vec![NAMESPACE.to_owned(), name.to_owned()])
        .expect("a namespace and an object name are non-empty")
}
