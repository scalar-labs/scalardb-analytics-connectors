//! The source metadata document of one table, and the table metadata derived
//! from it.

use aws_sdk_dynamodb::types::{self as sdk, TableDescription};
use connector_sdk::{
    Column, Constraint, ConstraintKind, Reported, TableKind, TableMetadata, TableReference,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ITEM, NAMESPACE, type_mapping::column_type};

/// What the connector read about one table: the parts of its `DescribeTable`
/// result that state what the table and its keys are.
///
/// It is the source metadata document. Item counts, sizes, the table's
/// status, and provisioned throughput change without any change to the
/// table's definition, so they are not read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DynamoDbTable {
    /// The table's name.
    pub name: String,
    /// The identifier DynamoDB gives the table when it is created; a table
    /// re-created under the same name has a new one.
    pub table_id: Option<String>,
    /// The table's Amazon Resource Name: the account, Region, and name.
    pub arn: Option<String>,
    /// The table's primary key: its partition key, then its sort key if any.
    pub key_schema: Vec<KeyElement>,
    /// The types of the attributes that key the table or its indexes. These
    /// are the only attributes whose type DynamoDB declares.
    pub attribute_definitions: Vec<AttributeDefinition>,
    /// The table's global secondary indexes.
    pub global_secondary_indexes: Vec<SecondaryIndex>,
    /// The table's local secondary indexes.
    pub local_secondary_indexes: Vec<SecondaryIndex>,
    /// How reads and writes are billed: `PROVISIONED` or `PAY_PER_REQUEST`.
    /// DynamoDB omits it for some tables that were always provisioned.
    pub billing_mode: Option<String>,
    /// The table class: `STANDARD` or `STANDARD_INFREQUENT_ACCESS`. DynamoDB
    /// omits it for tables that never changed from `STANDARD`.
    pub table_class: Option<String>,
    /// The table's stream, when one was ever configured.
    pub stream: Option<StreamSpecification>,
}

/// One attribute of a key schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KeyElement {
    /// The key attribute's name.
    pub attribute_name: String,
    /// The attribute's role in the key.
    pub key_type: KeyType,
}

/// The role of an attribute in a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum KeyType {
    /// The partition key (`HASH`).
    #[serde(rename = "HASH")]
    Hash,
    /// The sort key (`RANGE`).
    #[serde(rename = "RANGE")]
    Range,
}

/// The declared type of one key attribute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttributeDefinition {
    /// The attribute's name.
    pub attribute_name: String,
    /// The scalar type: `S` (string), `N` (number), or `B` (binary).
    pub attribute_type: String,
}

/// One global or local secondary index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecondaryIndex {
    /// The index's name.
    pub name: String,
    /// The index's key: its partition key, then its sort key if any.
    pub key_schema: Vec<KeyElement>,
    /// The attributes the index copies from the table.
    pub projection: Projection,
}

/// The attributes a secondary index copies from the table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Projection {
    /// `KEYS_ONLY`, `INCLUDE`, or `ALL`.
    pub projection_type: Option<String>,
    /// The non-key attributes an `INCLUDE` projection copies.
    pub non_key_attributes: Vec<String>,
}

/// The table's stream settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamSpecification {
    /// Whether the stream is enabled.
    pub enabled: bool,
    /// What each stream record holds, such as `NEW_AND_OLD_IMAGES`.
    pub view_type: Option<String>,
}

impl DynamoDbTable {
    /// Keeps the parts of a `DescribeTable` result that define the table.
    pub(crate) fn from_description(description: TableDescription) -> Self {
        Self {
            name: description.table_name.unwrap_or_default(),
            table_id: description.table_id,
            arn: description.table_arn,
            key_schema: key_elements(description.key_schema),
            attribute_definitions: description
                .attribute_definitions
                .unwrap_or_default()
                .into_iter()
                .map(|definition| AttributeDefinition {
                    attribute_name: definition.attribute_name,
                    attribute_type: definition.attribute_type.as_str().to_owned(),
                })
                .collect(),
            global_secondary_indexes: description
                .global_secondary_indexes
                .unwrap_or_default()
                .into_iter()
                .map(|index| {
                    SecondaryIndex::new(index.index_name, index.key_schema, index.projection)
                })
                .collect(),
            local_secondary_indexes: description
                .local_secondary_indexes
                .unwrap_or_default()
                .into_iter()
                .map(|index| {
                    SecondaryIndex::new(index.index_name, index.key_schema, index.projection)
                })
                .collect(),
            billing_mode: description
                .billing_mode_summary
                .and_then(|summary| summary.billing_mode)
                .map(|mode| mode.as_str().to_owned()),
            table_class: description
                .table_class_summary
                .and_then(|summary| summary.table_class)
                .map(|class| class.as_str().to_owned()),
            stream: description
                .stream_specification
                .map(|stream| StreamSpecification {
                    enabled: stream.stream_enabled,
                    view_type: stream.stream_view_type.map(|view| view.as_str().to_owned()),
                }),
        }
    }

    pub fn table(&self) -> TableReference {
        reference(&self.name)
    }

    /// The table's key attributes in column order: the partition key, then
    /// the sort key.
    pub fn keys(&self) -> impl Iterator<Item = &KeyElement> {
        let role = |key_type| {
            self.key_schema
                .iter()
                .filter(move |key| key.key_type == key_type)
        };
        role(KeyType::Hash).chain(role(KeyType::Range))
    }

    /// The declared type of the attribute named `name`, such as `S`.
    pub fn attribute_type(&self, name: &str) -> Option<&str> {
        self.attribute_definitions
            .iter()
            .find(|definition| definition.attribute_name == name)
            .map(|definition| definition.attribute_type.as_str())
    }

    /// Derives the table metadata: one NOT NULL column per key attribute,
    /// since every item has its key attributes, then the NOT NULL [`ITEM`]
    /// column holding each whole item, and the primary key.
    ///
    /// A table with a key attribute named [`ITEM`] has no whole-item column,
    /// since column names are unique and the key column keeps its name.
    ///
    /// DynamoDB does not name the primary key, so the constraint takes the
    /// table's name. Secondary indexes do not keep their keys unique, so they
    /// are no constraints; they stay in the source metadata.
    pub fn table_metadata(&self) -> TableMetadata {
        let whole_item = self.keys().all(|key| key.attribute_name != ITEM);
        let columns = self
            .keys()
            .map(|key| {
                let name = key.attribute_name.as_str();
                (name, self.attribute_type(name))
            })
            .chain(whole_item.then_some((ITEM, Some(ITEM))))
            .zip(1..)
            .map(|((name, data_source_type), ordinal)| Column {
                name: name.to_owned(),
                ordinal,
                nullable: false,
                source_id: Reported::NotOffered,
                ty: column_type(data_source_type),
            })
            .collect();
        let source_id = match &self.table_id {
            Some(id) => Reported::Value(id.clone()),
            None => Reported::Missing {
                reason: "DescribeTable returned no TableId".to_owned(),
            },
        };
        let primary_key = Constraint {
            name: self.name.clone(),
            enforced: true,
            kind: ConstraintKind::PrimaryKey {
                columns: self.keys().map(|key| key.attribute_name.clone()).collect(),
            },
        };
        TableMetadata::new(
            self.table(),
            TableKind::Table,
            source_id,
            columns,
            false,
            Reported::Value(vec![primary_key]),
        )
    }
}

impl SecondaryIndex {
    fn new(
        name: Option<String>,
        key_schema: Option<Vec<sdk::KeySchemaElement>>,
        projection: Option<sdk::Projection>,
    ) -> Self {
        Self {
            name: name.unwrap_or_default(),
            key_schema: key_elements(key_schema),
            projection: Projection::from_sdk(projection),
        }
    }
}

impl Projection {
    fn from_sdk(projection: Option<sdk::Projection>) -> Self {
        let projection = projection.unwrap_or_else(|| sdk::Projection::builder().build());
        Self {
            projection_type: projection
                .projection_type
                .map(|projection_type| projection_type.as_str().to_owned()),
            non_key_attributes: projection.non_key_attributes.unwrap_or_default(),
        }
    }
}

fn key_elements(key_schema: Option<Vec<sdk::KeySchemaElement>>) -> Vec<KeyElement> {
    key_schema
        .unwrap_or_default()
        .into_iter()
        .filter_map(|element| {
            let key_type = match element.key_type {
                sdk::KeyType::Hash => KeyType::Hash,
                sdk::KeyType::Range => KeyType::Range,
                _ => return None,
            };
            Some(KeyElement {
                attribute_name: element.attribute_name,
                key_type,
            })
        })
        .collect()
}

/// The table reference of the table named `name`.
pub(crate) fn reference(name: &str) -> TableReference {
    TableReference::new(vec![NAMESPACE.to_owned(), name.to_owned()])
        .expect("the namespace and a table name are non-empty")
}
