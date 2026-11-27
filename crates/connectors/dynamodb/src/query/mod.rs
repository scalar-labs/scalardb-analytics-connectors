//! The planned query and running it: one `Scan` of a table, or one `Query` of
//! a partition, what it projects from each item, and decoding its items into
//! Arrow batches.

mod item;

use std::{collections::HashMap, sync::Arc};

use analytics_types::{AnalyticsField, AnalyticsType, arrow_schema::Field, arrow_schema::Schema};
use arrow_array::{
    ArrayRef, RecordBatch, RecordBatchOptions,
    builder::{BinaryBuilder, StringBuilder},
};
use aws_sdk_dynamodb::{primitives::Blob, types::AttributeValue};
use connector_sdk::{
    Connector, ConnectorError, ErrorKind, ExecutionOutput, Executor, PortableQuery, Session,
};
use futures_util::stream;
use serde::{Deserialize, Serialize};

use crate::{DynamoDbConnector, Error, catalog::ITEM};

/// One read of a table implementing a remote plan: a `Scan` of the table,
/// or a `Query` of the items `key_condition` selects when it is not empty.
///
/// The read decodes `values` from each item, each once, and `columns` places
/// them in the remote output: output column `i` is `values[columns[i]]`.
///
/// It is one sequential segment of eventually consistent reads: a `Scan` is
/// never a point-in-time view of the table, and strongly consistent reads
/// would cost twice the read capacity without giving one.
#[derive(Clone, Debug, PartialEq)]
pub struct TableRead {
    /// The table's name.
    pub table: String,
    /// The key condition of a `Query`: the partition key's equality, then at
    /// most one comparison of the sort key. Empty for a `Scan`.
    pub key_condition: Vec<KeyComparison>,
    pub values: Vec<Projected>,
    pub columns: Vec<usize>,
}

/// One comparison of a key attribute with a value, in a `Query`'s key
/// condition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyComparison {
    /// The key attribute's name.
    pub attribute: String,
    pub comparison: Comparison,
    pub value: KeyValue,
}

/// A comparison a key condition can make, with the key attribute on its left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    Equal,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

/// A key attribute's value: a string or binary, the key types the type
/// mapping maps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyValue {
    S(String),
    B(Vec<u8>),
}

/// What a read decodes from each item.
#[derive(Clone, Debug, PartialEq)]
pub enum Projected {
    /// One attribute, by its DynamoDB name, decoded into the field's
    /// Analytics type.
    Attribute(AnalyticsField),
    /// The whole item, as the DynamoDB JSON of the `item` column.
    Item,
}

/// The serialized form of a [`TableRead`].
#[derive(Serialize, Deserialize)]
struct TableReadBytes {
    table: String,
    key_condition: Vec<KeyComparison>,
    values: Vec<ProjectedBytes>,
    columns: Vec<usize>,
}

/// The serialized form of a [`Projected`], with an attribute's field in
/// Arrow form.
#[derive(Serialize, Deserialize)]
enum ProjectedBytes {
    Attribute(Field),
    Item,
}

impl PortableQuery for TableRead {
    fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&TableReadBytes {
            table: self.table.clone(),
            key_condition: self.key_condition.clone(),
            values: self
                .values
                .iter()
                .map(|value| match value {
                    Projected::Attribute(field) => ProjectedBytes::Attribute(field.to_arrow()),
                    Projected::Item => ProjectedBytes::Item,
                })
                .collect(),
            columns: self.columns.clone(),
        })
        .expect("a table read serializes to JSON")
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError> {
        let invalid = || {
            ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "the bytes are not a DynamoDB table read",
            )
        };
        let read: TableReadBytes = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let values = read
            .values
            .iter()
            .map(|value| match value {
                ProjectedBytes::Attribute(field) => AnalyticsField::try_from_arrow(field)
                    .map(Projected::Attribute)
                    .map_err(|_| invalid()),
                ProjectedBytes::Item => Ok(Projected::Item),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if read.columns.iter().any(|column| *column >= values.len()) {
            return Err(invalid());
        }
        Ok(Self {
            table: read.table,
            key_condition: read.key_condition,
            values,
            columns: read.columns,
        })
    }
}

impl Projected {
    /// The field this value is decoded as.
    fn field(&self) -> AnalyticsField {
        match self {
            Self::Attribute(field) => field.clone(),
            Self::Item => AnalyticsField::new(ITEM, AnalyticsType::Json, false),
        }
    }
}

impl Comparison {
    fn operator(self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Greater => ">",
            Self::GreaterOrEqual => ">=",
        }
    }
}

impl KeyValue {
    fn attribute_value(&self) -> AttributeValue {
        match self {
            Self::S(text) => AttributeValue::S(text.clone()),
            Self::B(bytes) => AttributeValue::B(Blob::new(bytes.clone())),
        }
    }
}

/// The expressions every request of one read carries.
///
/// Names and values are placeholders, so that any attribute name and value
/// can be sent: `#a` for a projected attribute, `#k` and `:k` for a key
/// comparison's attribute and value. DynamoDB refuses an empty or unused
/// placeholder map, so each map is `None` when nothing uses it.
#[derive(Clone)]
struct Expressions {
    /// `None` when the read decodes whole items, which DynamoDB returns
    /// without a projection expression.
    projection: Option<String>,
    key_condition: Option<String>,
    names: Option<HashMap<String, String>>,
    values: Option<HashMap<String, AttributeValue>>,
}

impl TableRead {
    fn expressions(&self) -> Expressions {
        let mut names = HashMap::new();
        let projection = self
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| match value {
                Projected::Attribute(field) => {
                    let placeholder = format!("#a{index}");
                    names.insert(placeholder.clone(), field.name().to_owned());
                    Some(placeholder)
                }
                Projected::Item => None,
            })
            .collect::<Option<Vec<_>>>();
        if projection.is_none() {
            names.clear();
        }
        let mut values = HashMap::new();
        let key_condition = self
            .key_condition
            .iter()
            .enumerate()
            .map(|(index, key)| {
                names.insert(format!("#k{index}"), key.attribute.clone());
                values.insert(format!(":k{index}"), key.value.attribute_value());
                format!("#k{index} {} :k{index}", key.comparison.operator())
            })
            .collect::<Vec<_>>();
        Expressions {
            projection: projection.map(|placeholders| placeholders.join(", ")),
            key_condition: (!key_condition.is_empty()).then(|| key_condition.join(" AND ")),
            names: (!names.is_empty()).then_some(names),
            values: (!values.is_empty()).then_some(values),
        }
    }

    /// Decodes one page of items into the remote output's columns.
    fn decode(
        &self,
        schema: &Arc<Schema>,
        items: &[HashMap<String, AttributeValue>],
    ) -> Result<RecordBatch, ConnectorError> {
        let values = self
            .values
            .iter()
            .map(|value| match value {
                Projected::Attribute(field) => {
                    decode_attribute(field.name(), field.data_type(), field.is_nullable(), items)
                }
                Projected::Item => decode_items(items),
            })
            .collect::<Result<Vec<_>, _>>()?;
        RecordBatch::try_new_with_options(
            Arc::clone(schema),
            self.columns
                .iter()
                .map(|column| Arc::clone(&values[*column]))
                .collect(),
            &RecordBatchOptions::new().with_row_count(Some(items.len())),
        )
        .map_err(|_| {
            ConnectorError::new(
                ErrorKind::Internal,
                false,
                "the decoded items do not form the remote output",
            )
        })
    }
}

/// Writes every item as the DynamoDB JSON of the `item` column.
fn decode_items(items: &[HashMap<String, AttributeValue>]) -> Result<ArrayRef, ConnectorError> {
    let mut builder = StringBuilder::with_capacity(items.len(), 0);
    for item in items {
        let json = item::item_json(item).map_err(|_| {
            ConnectorError::new(
                ErrorKind::Data,
                false,
                "an item holds a value of a DynamoDB type the connector does not know",
            )
        })?;
        builder.append_value(json);
    }
    Ok(Arc::new(builder.finish()))
}

/// Decodes attribute `name` of every item as `data_type`.
///
/// An item without the attribute gives NULL where the column is nullable.
/// A value of another DynamoDB type is a `data` error, never coerced.
fn decode_attribute(
    name: &str,
    data_type: &AnalyticsType,
    nullable: bool,
    items: &[HashMap<String, AttributeValue>],
) -> Result<ArrayRef, ConnectorError> {
    let mismatch = |expected: &str| {
        ConnectorError::new(
            ErrorKind::Data,
            false,
            format!("a value of attribute {name} is not of DynamoDB type {expected}"),
        )
    };
    let absent = || {
        ConnectorError::new(
            ErrorKind::Data,
            false,
            format!("an item has no attribute {name}, which the column requires"),
        )
    };
    Ok(match data_type {
        AnalyticsType::Text => {
            let mut builder = StringBuilder::with_capacity(items.len(), 0);
            for item in items {
                match item.get(name) {
                    Some(AttributeValue::S(value)) => builder.append_value(value),
                    Some(_) => return Err(mismatch("S")),
                    None if nullable => builder.append_null(),
                    None => return Err(absent()),
                }
            }
            Arc::new(builder.finish())
        }
        AnalyticsType::Blob => {
            let mut builder = BinaryBuilder::with_capacity(items.len(), 0);
            for item in items {
                match item.get(name) {
                    Some(AttributeValue::B(value)) => builder.append_value(value.as_ref()),
                    Some(_) => return Err(mismatch("B")),
                    None if nullable => builder.append_null(),
                    None => return Err(absent()),
                }
            }
            Arc::new(builder.finish())
        }
        _ => {
            return Err(ConnectorError::new(
                ErrorKind::Internal,
                false,
                "the scan projects an attribute of a type the connector does not decode",
            ));
        }
    })
}

/// Runs a planned read through the SDK.
pub struct DynamoDbExecutor;

/// Where the next page starts: the first page, after an evaluated key, or
/// nowhere once the last page was read.
enum Page {
    First,
    After(HashMap<String, AttributeValue>),
    Done,
}

impl Executor<DynamoDbConnector> for DynamoDbExecutor {
    /// Returns one batch per non-empty `Scan` or `Query` page, with the
    /// remote output's columns in order. Each page is requested when the
    /// previous one has been pulled, starting after its `LastEvaluatedKey`.
    fn run(
        session: &Session<DynamoDbConnector>,
        query: &TableRead,
    ) -> Result<ExecutionOutput, ConnectorError> {
        let schema = Arc::new(Schema::new(
            query
                .columns
                .iter()
                .map(|column| query.values[*column].field().to_arrow())
                .collect::<Vec<_>>(),
        ));
        let expressions = query.expressions();
        let client = session.access().client.clone();
        let query = query.clone();
        let batches = stream::try_unfold(Page::First, move |page| {
            let client = client.clone();
            let schema = Arc::clone(&schema);
            let expressions = expressions.clone();
            let query = query.clone();
            async move {
                let mut page = page;
                loop {
                    let start = match page {
                        Page::First => None,
                        Page::After(key) => Some(key),
                        Page::Done => return Ok(None),
                    };
                    let (items, last_evaluated_key) = match expressions.key_condition.clone() {
                        None => {
                            let output = client
                                .scan()
                                .table_name(&query.table)
                                .set_projection_expression(expressions.projection.clone())
                                .set_expression_attribute_names(expressions.names.clone())
                                .consistent_read(false)
                                .set_exclusive_start_key(start)
                                .send()
                                .await
                                .map_err(|error| DynamoDbConnector::classify(Error::from(error)))?;
                            (output.items, output.last_evaluated_key)
                        }
                        Some(key_condition) => {
                            let output = client
                                .query()
                                .table_name(&query.table)
                                .key_condition_expression(key_condition)
                                .set_projection_expression(expressions.projection.clone())
                                .set_expression_attribute_names(expressions.names.clone())
                                .set_expression_attribute_values(expressions.values.clone())
                                .consistent_read(false)
                                .set_exclusive_start_key(start)
                                .send()
                                .await
                                .map_err(|error| DynamoDbConnector::classify(Error::from(error)))?;
                            (output.items, output.last_evaluated_key)
                        }
                    };
                    page = match last_evaluated_key {
                        Some(key) if !key.is_empty() => Page::After(key),
                        _ => Page::Done,
                    };
                    let items = items.unwrap_or_default();
                    if !items.is_empty() {
                        return Ok(Some((query.decode(&schema, &items)?, page)));
                    }
                }
            }
        });
        Ok(session.access().source_read(batches))
    }
}
