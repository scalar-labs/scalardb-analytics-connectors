//! The three metadata requests and their results as Arrow rows.
//!
//! Each request kind has one fixed output schema, so a stream can return it
//! before the Data Source is queried. Every pulled item becomes one row: a
//! namespace or table of a list, an unreadable range, or one requested table's
//! record. The schemas are those of the contract's Connector API section.

use std::{
    pin::Pin,
    sync::{Arc, LazyLock},
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use analytics_types::{AnalyticsField, AnalyticsType};
use arrow_array::{
    Array, ArrayRef, BinaryArray, BooleanArray, ListArray, RecordBatch, RecordBatchReader,
    StringArray, StructArray, TimestampMicrosecondArray, UInt32Array,
};
use arrow_buffer::{NullBuffer, OffsetBuffer};
use arrow_ipc::writer::{DictionaryTracker, IpcDataGenerator, IpcWriteOptions, write_message};
use arrow_schema::{DataType, Field, FieldRef, Fields, Schema, SchemaRef, TimeUnit};
use futures_core::Stream;

use crate::{
    Column, ColumnType, Component, ConnectorError, Constraint, ConstraintKind, ErrorKind, Listed,
    NameFilter, NamespacePath, Reported, SourceStream, SourceTableMetadata, TableEntry, TableKind,
    TableMetadata, TableRecord, TableReference, UniqueKind, UnsupportedReason,
};

/// One metadata request as the caller sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataRequest {
    /// Namespaces whose names exactly match the filter.
    NamespaceList { namespaces: NameFilter },
    /// Tables of the matching namespaces whose names exactly match `tables`.
    TableList {
        namespaces: NameFilter,
        tables: NameFilter,
    },
    /// One record per named table.
    TableMetadata { tables: Vec<TableReference> },
}

impl MetadataRequest {
    /// Returns the fixed schema of this request kind's rows.
    pub fn output_schema(&self) -> SchemaRef {
        match self {
            Self::NamespaceList { .. } => Arc::clone(&NAMESPACE_LIST_SCHEMA),
            Self::TableList { .. } => Arc::clone(&TABLE_LIST_SCHEMA),
            Self::TableMetadata { .. } => Arc::clone(&TABLE_METADATA_SCHEMA),
        }
    }
}

/// The components that produced a table metadata result, carried in each row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Retrieval {
    pub(super) connector: Component,
    pub(super) adbc_driver: Option<Component>,
    pub(super) library_version: String,
}

/// Reads the tables named in a table metadata request from Arrow rows.
///
/// The rows have one column `table` of type `list<utf8>`; every row is a
/// non-null, non-empty name path of non-null, non-empty names. Anything else
/// is an invalid request.
pub fn read_table_input(
    reader: impl RecordBatchReader,
) -> Result<Vec<TableReference>, ConnectorError> {
    let schema = reader.schema();
    let is_name_path = |field: &Field| {
        field.name() == "table"
            && matches!(field.data_type(), DataType::List(item) if item.data_type() == &DataType::Utf8)
    };
    match schema.fields().as_ref() {
        [field] if is_name_path(field) => {}
        _ => {
            return Err(invalid_input(
                "the table input must have one column `table` of type list<utf8>",
            ));
        }
    }
    let mut tables = Vec::new();
    for batch in reader {
        // A C caller's arrays are imported without checks; their offsets and
        // UTF-8 are validated before any value is read.
        let batch = batch
            .and_then(|batch| {
                StructArray::from(batch.clone()).to_data().validate_full()?;
                Ok(batch)
            })
            .map_err(|error| {
                invalid_input(&format!("the table input could not be read: {error}"))
            })?;
        let column = batch
            .column(0)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| invalid_input("the table input column is not a list"))?;
        for row in 0..column.len() {
            if column.is_null(row) {
                return Err(invalid_input("a table in the table input is null"));
            }
            let values = column.value(row);
            let names = values
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| invalid_input("a table name is not text"))?
                .iter()
                .map(|name| {
                    name.map(str::to_owned)
                        .ok_or_else(|| invalid_input("a table name is null"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            tables.push(
                TableReference::new(names).map_err(|error| invalid_input(&error.to_string()))?,
            );
        }
    }
    Ok(tables)
}

fn invalid_input(message: &str) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidRequest, false, message)
}

/// A catalog read whose items become one-row batches as they are pulled.
///
/// Like the catalog read, it ends at its first error.
pub(super) struct MetadataRows<T> {
    items: SourceStream<T>,
    row: Box<dyn Fn(T) -> Result<RecordBatch, ConnectorError> + Send>,
    terminated: bool,
}

impl<T> MetadataRows<T> {
    pub(super) fn new(
        items: SourceStream<T>,
        row: impl Fn(T) -> Result<RecordBatch, ConnectorError> + Send + 'static,
    ) -> Self {
        Self {
            items,
            row: Box::new(row),
            terminated: false,
        }
    }
}

impl<T> Stream for MetadataRows<T> {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.terminated {
            return Poll::Ready(None);
        }
        match Pin::new(&mut this.items).poll_next(context) {
            Poll::Ready(Some(Ok(item))) => {
                let row = (this.row)(item);
                this.terminated = row.is_err();
                Poll::Ready(Some(row))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// Value spellings of the enumerated columns, as the contract records them.

const LISTED: &str = "listed";
const UNREADABLE: &str = "unreadable";

/// Returns the `kind` value and, for another kind, the source's own term.
fn kind_value(kind: &TableKind) -> (&'static str, Option<&str>) {
    match kind {
        TableKind::Table => ("table", None),
        TableKind::View => ("view", None),
        TableKind::External => ("external", None),
        TableKind::Other(name) => ("other", Some(name)),
    }
}

fn unsupported_reason_value(reason: UnsupportedReason) -> &'static str {
    match reason {
        UnsupportedReason::NotMapped => "not_mapped",
        UnsupportedReason::MappingNotImplemented => "mapping_not_implemented",
        UnsupportedReason::ParameterOutOfRange => "parameter_out_of_range",
    }
}

fn unique_kind_value(kind: UniqueKind) -> &'static str {
    match kind {
        UniqueKind::Constraint => "constraint",
        UniqueKind::Index => "index",
    }
}

// Column types shared by the schemas.

/// A name path, such as a namespace or a table: a list of non-null names.
fn names_type() -> DataType {
    DataType::List(name_item())
}

fn name_item() -> FieldRef {
    Arc::new(Field::new("item", DataType::Utf8, false))
}

fn names_field(name: &str, nullable: bool) -> Field {
    Field::new(name, names_type(), nullable)
}

fn text_field(name: &str, nullable: bool) -> Field {
    Field::new(name, DataType::Utf8, nullable)
}

/// An item that was read (`value`), could not be read (`missing`), or is never
/// offered (`not_offered`).
fn reported_fields(value: DataType) -> Fields {
    Fields::from(vec![
        text_field("state", false),
        Field::new("value", value, true),
        text_field("reason", true),
    ])
}

fn component_fields() -> Fields {
    Fields::from(vec![
        text_field("name", false),
        text_field("version", false),
    ])
}

fn column_type_fields() -> Fields {
    Fields::from(vec![
        text_field("state", false),
        text_field("data_source_type", true),
        Field::new("analytics_type", DataType::Binary, true),
        text_field("unsupported_reason", true),
        text_field("unread_reason", true),
    ])
}

fn column_fields() -> Fields {
    Fields::from(vec![
        text_field("name", false),
        Field::new("ordinal", DataType::UInt32, false),
        Field::new("nullable", DataType::Boolean, false),
        Field::new(
            "source_id",
            DataType::Struct(reported_fields(DataType::Utf8)),
            false,
        ),
        Field::new("type", DataType::Struct(column_type_fields()), false),
    ])
}

fn constraint_fields() -> Fields {
    Fields::from(vec![
        text_field("name", false),
        Field::new("enforced", DataType::Boolean, false),
        text_field("kind", false),
        names_field("columns", true),
        text_field("unique_kind", true),
        text_field("condition", true),
        names_field("referenced_table", true),
        names_field("referenced_columns", true),
        text_field("expression", true),
    ])
}

/// The non-null item field of a list of structs.
fn struct_item(fields: Fields) -> FieldRef {
    Arc::new(Field::new("item", DataType::Struct(fields), false))
}

fn constraints_type() -> DataType {
    DataType::List(struct_item(constraint_fields()))
}

fn retrieval_fields() -> Fields {
    Fields::from(vec![
        Field::new("connector", DataType::Struct(component_fields()), false),
        Field::new("adbc_driver", DataType::Struct(component_fields()), true),
        text_field("library_version", false),
        Field::new("read_at", read_at_type(), true),
    ])
}

fn read_at_type() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
}

static NAMESPACE_LIST_SCHEMA: LazyLock<SchemaRef> = LazyLock::new(|| {
    Arc::new(Schema::new(vec![
        text_field("state", false),
        names_field("namespace", true),
        names_field("scope", true),
        text_field("reason", true),
    ]))
});

static TABLE_LIST_SCHEMA: LazyLock<SchemaRef> = LazyLock::new(|| {
    Arc::new(Schema::new(vec![
        text_field("state", false),
        names_field("table", true),
        text_field("kind", true),
        text_field("source_kind", true),
        names_field("scope", true),
        text_field("reason", true),
    ]))
});

static TABLE_METADATA_SCHEMA: LazyLock<SchemaRef> = LazyLock::new(|| {
    Arc::new(Schema::new(vec![
        text_field("state", false),
        names_field("table", false),
        text_field("reason", true),
        text_field("kind", true),
        text_field("source_kind", true),
        Field::new(
            "source_id",
            DataType::Struct(reported_fields(DataType::Utf8)),
            true,
        ),
        Field::new(
            "columns",
            DataType::List(struct_item(column_fields())),
            true,
        ),
        Field::new("columns_may_be_hidden", DataType::Boolean, true),
        Field::new(
            "constraints",
            DataType::Struct(reported_fields(constraints_type())),
            true,
        ),
        text_field("source_metadata", true),
        Field::new("retrieval", DataType::Struct(retrieval_fields()), false),
    ]))
});

// Rows.

/// The row of one namespace list item.
pub(super) fn namespace_row(item: Listed<NamespacePath>) -> Result<RecordBatch, ConnectorError> {
    let (state, namespace, scope, reason) = match &item {
        Listed::Item(namespace) => (LISTED, Some(namespace.names.as_slice()), None, None),
        Listed::Unreadable { scope, reason } => (
            UNREADABLE,
            None,
            Some(scope.names.as_slice()),
            Some(reason.as_str()),
        ),
    };
    batch(
        &NAMESPACE_LIST_SCHEMA,
        vec![
            texts([Some(state)]),
            names([namespace]),
            names([scope]),
            texts([reason]),
        ],
    )
}

/// The row of one table list item.
pub(super) fn table_entry_row(item: Listed<TableEntry>) -> Result<RecordBatch, ConnectorError> {
    let columns = match &item {
        Listed::Item(entry) => {
            let (kind, source_kind) = kind_value(&entry.kind);
            vec![
                texts([Some(LISTED)]),
                names([Some(entry.table.names())]),
                texts([Some(kind)]),
                texts([source_kind]),
                names([None]),
                texts([None]),
            ]
        }
        Listed::Unreadable { scope, reason } => vec![
            texts([Some(UNREADABLE)]),
            names([None]),
            texts([None]),
            texts([None]),
            names([Some(scope.names.as_slice())]),
            texts([Some(reason.as_str())]),
        ],
    };
    batch(&TABLE_LIST_SCHEMA, columns)
}

/// The row of one requested table's record.
///
/// A table that was read carries the table metadata its connector derives and
/// its source metadata as JSON.
pub(super) fn table_row<T: SourceTableMetadata>(
    record: TableRecord<T>,
    retrieval: &Retrieval,
) -> Result<RecordBatch, ConnectorError> {
    let columns = match &record {
        TableRecord::Read { metadata, read_at } => {
            let source_metadata = serde_json::to_string(metadata).map_err(|_| {
                ConnectorError::new(
                    ErrorKind::Internal,
                    false,
                    "the source metadata could not be serialized as JSON",
                )
            })?;
            read_table_columns(
                &metadata.to_table_metadata(),
                &source_metadata,
                retrieval,
                *read_at,
            )?
        }
        TableRecord::NotFound { table } => {
            unread_table_columns("does_not_exist", table, None, retrieval)?
        }
        TableRecord::Unavailable { table, reason } => {
            unread_table_columns("could_not_be_read", table, Some(reason), retrieval)?
        }
    };
    batch(&TABLE_METADATA_SCHEMA, columns)
}

fn read_table_columns(
    metadata: &TableMetadata,
    source_metadata: &str,
    retrieval: &Retrieval,
    read_at: SystemTime,
) -> Result<Vec<ArrayRef>, ConnectorError> {
    let state = if metadata.is_complete() {
        "complete"
    } else {
        "incomplete"
    };
    let (kind, source_kind) = kind_value(metadata.kind());
    Ok(vec![
        texts([Some(state)]),
        names([Some(metadata.table().names())]),
        texts([None]),
        texts([Some(kind)]),
        texts([source_kind]),
        Arc::new(reported_texts(&[Some(metadata.source_id())])),
        Arc::new(columns_list(Some(metadata.columns()))?),
        Arc::new(BooleanArray::from(vec![Some(
            metadata.columns_may_be_hidden(),
        )])),
        Arc::new(constraints_struct(Some(metadata.constraints()))),
        texts([Some(source_metadata)]),
        Arc::new(retrieval_struct(retrieval, Some(read_at))),
    ])
}

/// The columns of a table that does not exist or could not be read: its name,
/// state, and reason, with no table metadata or source metadata.
fn unread_table_columns(
    state: &str,
    table: &TableReference,
    reason: Option<&str>,
    retrieval: &Retrieval,
) -> Result<Vec<ArrayRef>, ConnectorError> {
    Ok(vec![
        texts([Some(state)]),
        names([Some(table.names())]),
        texts([reason]),
        texts([None]),
        texts([None]),
        Arc::new(reported_texts(&[None])),
        Arc::new(columns_list(None)?),
        Arc::new(BooleanArray::from(vec![None::<bool>])),
        Arc::new(constraints_struct(None)),
        texts([None]),
        Arc::new(retrieval_struct(retrieval, None)),
    ])
}

fn batch(schema: &SchemaRef, columns: Vec<ArrayRef>) -> Result<RecordBatch, ConnectorError> {
    RecordBatch::try_new(Arc::clone(schema), columns).map_err(|error| {
        ConnectorError::new(
            ErrorKind::Internal,
            false,
            format!("a metadata row does not match its schema: {error}"),
        )
    })
}

// Arrays. Each builder takes the values of every row of one column. A null
// struct or list keeps its children null at the same position, as Arrow
// requires for children whose fields are not nullable.

fn texts<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> ArrayRef {
    Arc::new(StringArray::from_iter(values))
}

fn names<'a>(values: impl IntoIterator<Item = Option<&'a [String]>>) -> ArrayRef {
    Arc::new(names_array(values))
}

fn names_array<'a>(values: impl IntoIterator<Item = Option<&'a [String]>>) -> ListArray {
    let values: Vec<_> = values.into_iter().collect();
    let lengths: Vec<_> = values.iter().map(|names| names.map(<[_]>::len)).collect();
    let flat = texts(
        values
            .iter()
            .flatten()
            .flat_map(|names| names.iter().map(|name| Some(name.as_str()))),
    );
    list_array(name_item(), &lengths, flat)
}

/// A list array with one list per entry of `lengths`; `None` is a null list.
fn list_array(item: FieldRef, lengths: &[Option<usize>], values: ArrayRef) -> ListArray {
    ListArray::new(
        item,
        OffsetBuffer::from_lengths(lengths.iter().map(|length| length.unwrap_or(0))),
        values,
        validity(lengths.iter().map(Option::is_some)),
    )
}

/// A struct array; `present[i]` is false for a null struct.
fn struct_array(fields: Fields, arrays: Vec<ArrayRef>, present: &[bool]) -> StructArray {
    StructArray::new(fields, arrays, validity(present.iter().copied()))
}

/// The validity of a nullable array, omitted when every value is present.
fn validity(present: impl IntoIterator<Item = bool>) -> Option<NullBuffer> {
    let present: Vec<bool> = present.into_iter().collect();
    present
        .iter()
        .any(|present| !present)
        .then(|| NullBuffer::from(present))
}

/// Reported text items, such as source identifiers; `None` is a null item.
fn reported_texts(items: &[Option<&Reported<String>>]) -> StructArray {
    struct_array(
        reported_fields(DataType::Utf8),
        vec![
            texts(items.iter().map(|item| item.map(reported_state))),
            texts(
                items
                    .iter()
                    .map(|item| reported_value(*item).map(String::as_str)),
            ),
            texts(items.iter().map(|item| reported_reason(*item))),
        ],
        &items.iter().map(Option::is_some).collect::<Vec<_>>(),
    )
}

fn reported_state<T>(item: &Reported<T>) -> &'static str {
    match item {
        Reported::Value(_) => "value",
        Reported::Missing { .. } => "missing",
        Reported::NotOffered => "not_offered",
    }
}

fn reported_value<T>(item: Option<&Reported<T>>) -> Option<&T> {
    match item {
        Some(Reported::Value(value)) => Some(value),
        _ => None,
    }
}

fn reported_reason<T>(item: Option<&Reported<T>>) -> Option<&str> {
    match item {
        Some(Reported::Missing { reason }) => Some(reason),
        _ => None,
    }
}

fn columns_list(columns: Option<&[Column]>) -> Result<ListArray, ConnectorError> {
    let all = columns.unwrap_or_default();
    let mut type_states = Vec::new();
    let mut data_source_types = Vec::new();
    let mut analytics_types = Vec::new();
    let mut unsupported_reasons = Vec::new();
    let mut unread_reasons = Vec::new();
    for column in all {
        let (state, data_source_type, analytics_type, unsupported, unread) = match &column.ty {
            ColumnType::Mapped {
                data_source_type,
                analytics_type,
            } => (
                "mapped",
                Some(data_source_type.as_str()),
                Some(analytics_type_bytes(analytics_type)?),
                None,
                None,
            ),
            ColumnType::Unsupported {
                data_source_type,
                reason,
            } => (
                "unsupported",
                Some(data_source_type.as_str()),
                None,
                Some(unsupported_reason_value(*reason)),
                None,
            ),
            ColumnType::Unread { reason } => ("unread", None, None, None, Some(reason.as_str())),
        };
        type_states.push(Some(state));
        data_source_types.push(data_source_type);
        analytics_types.push(analytics_type);
        unsupported_reasons.push(unsupported);
        unread_reasons.push(unread);
    }
    let every = vec![true; all.len()];
    let column_types = struct_array(
        column_type_fields(),
        vec![
            texts(type_states),
            texts(data_source_types),
            Arc::new(BinaryArray::from_iter(analytics_types)),
            texts(unsupported_reasons),
            texts(unread_reasons),
        ],
        &every,
    );
    let source_ids: Vec<_> = all.iter().map(|column| Some(&column.source_id)).collect();
    let columns_struct = struct_array(
        column_fields(),
        vec![
            texts(all.iter().map(|column| Some(column.name.as_str()))),
            Arc::new(UInt32Array::from_iter_values(
                all.iter().map(|column| column.ordinal),
            )),
            Arc::new(BooleanArray::from_iter(
                all.iter().map(|column| Some(column.nullable)),
            )),
            Arc::new(reported_texts(&source_ids)),
            Arc::new(column_types),
        ],
        &every,
    );
    Ok(list_array(
        struct_item(column_fields()),
        &[columns.map(<[_]>::len)],
        Arc::new(columns_struct),
    ))
}

/// The reported constraints of one row; `None` is a null item.
fn constraints_struct(constraints: Option<&Reported<Vec<Constraint>>>) -> StructArray {
    let value = reported_value(constraints);
    let all = value.map(Vec::as_slice).unwrap_or_default();
    let mut kinds = Vec::new();
    let mut columns = Vec::new();
    let mut unique_kinds = Vec::new();
    let mut conditions = Vec::new();
    let mut referenced_tables = Vec::new();
    let mut referenced_columns = Vec::new();
    let mut expressions = Vec::new();
    for constraint in all {
        let (kind, constrained, unique_kind, condition, referenced_table, referenced, expression) =
            match &constraint.kind {
                ConstraintKind::PrimaryKey { columns } => (
                    "primary_key",
                    Some(columns.as_slice()),
                    None,
                    None,
                    None,
                    None,
                    None,
                ),
                ConstraintKind::Unique {
                    columns,
                    kind,
                    condition,
                } => (
                    "unique",
                    Some(columns.as_slice()),
                    Some(unique_kind_value(*kind)),
                    condition.as_deref(),
                    None,
                    None,
                    None,
                ),
                ConstraintKind::ForeignKey {
                    columns,
                    referenced_table,
                    referenced_columns,
                } => (
                    "foreign_key",
                    Some(columns.as_slice()),
                    None,
                    None,
                    Some(referenced_table.names()),
                    Some(referenced_columns.as_slice()),
                    None,
                ),
                ConstraintKind::Check { expression } => (
                    "check",
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(expression.as_str()),
                ),
            };
        kinds.push(Some(kind));
        columns.push(constrained);
        unique_kinds.push(unique_kind);
        conditions.push(condition);
        referenced_tables.push(referenced_table);
        referenced_columns.push(referenced);
        expressions.push(expression);
    }
    let constraint_values = struct_array(
        constraint_fields(),
        vec![
            texts(all.iter().map(|constraint| Some(constraint.name.as_str()))),
            Arc::new(BooleanArray::from_iter(
                all.iter().map(|constraint| Some(constraint.enforced)),
            )),
            texts(kinds),
            names(columns),
            texts(unique_kinds),
            texts(conditions),
            names(referenced_tables),
            names(referenced_columns),
            texts(expressions),
        ],
        &vec![true; all.len()],
    );
    let list = list_array(
        struct_item(constraint_fields()),
        &[value.map(Vec::len)],
        Arc::new(constraint_values),
    );
    struct_array(
        reported_fields(constraints_type()),
        vec![
            texts([constraints.map(reported_state)]),
            Arc::new(list),
            texts([reported_reason(constraints)]),
        ],
        &[constraints.is_some()],
    )
}

fn retrieval_struct(retrieval: &Retrieval, read_at: Option<SystemTime>) -> StructArray {
    let component = |component: Option<&Component>| {
        struct_array(
            component_fields(),
            vec![
                texts([component.map(|component| component.name.as_str())]),
                texts([component.map(|component| component.version.as_str())]),
            ],
            &[component.is_some()],
        )
    };
    struct_array(
        retrieval_fields(),
        vec![
            Arc::new(component(Some(&retrieval.connector))),
            Arc::new(component(retrieval.adbc_driver.as_ref())),
            texts([Some(retrieval.library_version.as_str())]),
            Arc::new(
                TimestampMicrosecondArray::from(vec![read_at.map(micros_since_epoch)])
                    .with_timezone("UTC"),
            ),
        ],
        &[true],
    )
}

/// Encodes an Analytics type as the Arrow IPC schema message of one field.
///
/// The message is encapsulated (continuation marker, length, flatbuffer), as an
/// IPC stream begins and as Arrow Flight carries schemas. The field's name is
/// empty and it is nullable: the column's name and nullability are items of
/// the row itself.
fn analytics_type_bytes(analytics_type: &AnalyticsType) -> Result<Vec<u8>, ConnectorError> {
    let field = AnalyticsField::new("", analytics_type.clone(), true).to_arrow();
    let schema = Schema::new(vec![field]);
    let options = IpcWriteOptions::default();
    let encoded = IpcDataGenerator::default().schema_to_bytes_with_dictionary_tracker(
        &schema,
        &mut DictionaryTracker::new(false),
        &options,
    );
    let mut bytes = Vec::new();
    write_message(&mut bytes, encoded, &options).map_err(|error| {
        ConnectorError::new(
            ErrorKind::Internal,
            false,
            format!("an Analytics type could not be encoded: {error}"),
        )
    })?;
    Ok(bytes)
}

fn micros_since_epoch(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => saturating_micros(after),
        Err(before) => -saturating_micros(before.duration()),
    }
}

fn saturating_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}
