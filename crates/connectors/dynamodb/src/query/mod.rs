//! The planned query and running it: one `Scan` of a table, what it projects
//! from each item, and decoding its items into Arrow batches.

mod item;

use std::{collections::HashMap, sync::Arc};

use analytics_types::{AnalyticsField, AnalyticsType, arrow_schema::Field, arrow_schema::Schema};
use arrow_array::{
    ArrayRef, RecordBatch, RecordBatchOptions,
    builder::{BinaryBuilder, StringBuilder},
};
use aws_sdk_dynamodb::types::AttributeValue;
use connector_sdk::{
    Connector, ConnectorError, ErrorKind, ExecutionOutput, Executor, PortableQuery, Session,
};
use futures_util::stream;
use serde::{Deserialize, Serialize};

use crate::{DynamoDbConnector, Error, catalog::ITEM};

/// One read of a table implementing a remote plan.
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
    pub values: Vec<Projected>,
    pub columns: Vec<usize>,
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

impl TableRead {
    /// The projection expression, with a placeholder per attribute so that
    /// any attribute name can be projected, and the names the placeholders
    /// stand for; `None` when the read decodes whole items, which DynamoDB
    /// returns without one.
    fn projection(&self) -> Option<(String, HashMap<String, String>)> {
        let mut names = HashMap::new();
        let mut placeholders = Vec::new();
        for (index, value) in self.values.iter().enumerate() {
            match value {
                Projected::Attribute(field) => {
                    let placeholder = format!("#a{index}");
                    names.insert(placeholder.clone(), field.name().to_owned());
                    placeholders.push(placeholder);
                }
                Projected::Item => return None,
            }
        }
        Some((placeholders.join(", "), names))
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

/// Where the next `Scan` page starts: the first page, after an evaluated key,
/// or nowhere once the last page was read.
enum Page {
    First,
    After(HashMap<String, AttributeValue>),
    Done,
}

impl Executor<DynamoDbConnector> for DynamoDbExecutor {
    /// Returns one batch per non-empty `Scan` page, with the remote output's
    /// columns in order. Each page is requested when the previous one has
    /// been pulled, starting after its `LastEvaluatedKey`.
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
        let (expression, names) = query.projection().unzip();
        let client = session.access().client.clone();
        let query = query.clone();
        let batches = stream::try_unfold(Page::First, move |page| {
            let client = client.clone();
            let schema = Arc::clone(&schema);
            let expression = expression.clone();
            let names = names.clone();
            let query = query.clone();
            async move {
                let mut page = page;
                loop {
                    let start = match page {
                        Page::First => None,
                        Page::After(key) => Some(key),
                        Page::Done => return Ok(None),
                    };
                    let output = client
                        .scan()
                        .table_name(&query.table)
                        .set_projection_expression(expression.clone())
                        .set_expression_attribute_names(names.clone())
                        .consistent_read(false)
                        .set_exclusive_start_key(start)
                        .send()
                        .await
                        .map_err(|error| DynamoDbConnector::classify(Error::from(error)))?;
                    page = match output.last_evaluated_key {
                        Some(key) if !key.is_empty() => Page::After(key),
                        _ => Page::Done,
                    };
                    let items = output.items.unwrap_or_default();
                    if !items.is_empty() {
                        return Ok(Some((query.decode(&schema, &items)?, page)));
                    }
                }
            }
        });
        Ok(session.access().source_read(batches))
    }
}
