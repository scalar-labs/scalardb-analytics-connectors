//! The C representation of metadata: namespace and table lists, table
//! records with their table metadata and source metadata, and Analytics types.
//!
//! Each structure mirrors one SDK type. The library writes trees of these
//! structures as outputs; it reads the names of requested tables as input.
//!
//! Every enumerated field and truth value is an integer type with named
//! constants rather than a Rust enum or `bool`, so the structures hold only
//! values a C caller can write. A field that its structure's state does not
//! use is zero or absent on output. Text inside these structures is a
//! [`ScalardbAnalyticsConnectorString`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use analytics_types::AnalyticsType;
use connector_sdk::{
    Column, ColumnType, Component, ConnectorError, Constraint, ConstraintKind, Listed,
    NamespacePath, Reported, TableEntry, TableKind, TableMetadata, TableRecord, TableReference,
    UniqueKind, UnsupportedReason,
    runtime::{Retrieval, SourceMetadata, TableMetadataResult, TableRead},
};

use super::{invalid_request, owned::Arena, read_slice, text::ScalardbAnalyticsConnectorString};

/// A sequence of names: a namespace, a table, or a list of column names.
///
/// For a namespace or a table of a source with catalogs, the first name is
/// the catalog.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorNames {
    pub names: *const ScalardbAnalyticsConnectorString,
    pub name_count: usize,
}

/// A truth value in a structure.
///
/// It is an integer with named constants, like the enumerated fields.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorBool(pub u8);

impl ScalardbAnalyticsConnectorBool {
    pub const FALSE: Self = Self(0);
    pub const TRUE: Self = Self(1);
}

impl From<bool> for ScalardbAnalyticsConnectorBool {
    fn from(value: bool) -> Self {
        if value { Self::TRUE } else { Self::FALSE }
    }
}

/// Whether a list entry is an item or a range whose list could not be read.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorListedState(pub u32);

impl ScalardbAnalyticsConnectorListedState {
    pub const ITEM: Self = Self(1);
    /// No targets are returned for the range; this does not mean it is empty.
    pub const UNREADABLE: Self = Self(2);
}

/// One namespace list entry. `namespace` is set for an item; `scope` and
/// `reason` for an unreadable range.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorNamespaceEntry {
    pub state: ScalardbAnalyticsConnectorListedState,
    pub namespace: ScalardbAnalyticsConnectorNames,
    pub scope: ScalardbAnalyticsConnectorNames,
    pub reason: ScalardbAnalyticsConnectorString,
}

/// The result of a namespace list, freed with
/// `scalardb_analytics_connector_namespace_list_free`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorNamespaceList {
    pub entries: *const ScalardbAnalyticsConnectorNamespaceEntry,
    pub entry_count: usize,
}

/// The kind of table the source distinguishes.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorTableKind(pub u32);

impl ScalardbAnalyticsConnectorTableKind {
    pub const TABLE: Self = Self(1);
    pub const VIEW: Self = Self(2);
    pub const EXTERNAL: Self = Self(3);
    /// Another kind, named in the source's own term by `other_kind`.
    pub const OTHER: Self = Self(4);
}

/// One table list entry. `table`, `kind`, and `other_kind` are set for an
/// item; `scope` and `reason` for an unreadable range.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorTableEntry {
    pub state: ScalardbAnalyticsConnectorListedState,
    pub table: ScalardbAnalyticsConnectorNames,
    pub kind: ScalardbAnalyticsConnectorTableKind,
    pub other_kind: ScalardbAnalyticsConnectorString,
    pub scope: ScalardbAnalyticsConnectorNames,
    pub reason: ScalardbAnalyticsConnectorString,
}

/// The result of a table list, freed with
/// `scalardb_analytics_connector_table_list_free`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorTableList {
    pub entries: *const ScalardbAnalyticsConnectorTableEntry,
    pub entry_count: usize,
}

/// Whether the connector read an item, could not read it, or never offers it.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorReportedState(pub u32);

impl ScalardbAnalyticsConnectorReportedState {
    pub const VALUE: Self = Self(1);
    /// The item exists in principle but could not be read; the table is incomplete.
    pub const MISSING: Self = Self(2);
    /// The connector never returns the item; the table stays complete.
    pub const NOT_OFFERED: Self = Self(3);
}

/// A reported text item: `value` is set for `VALUE`, `missing_reason` for
/// `MISSING`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorReportedText {
    pub state: ScalardbAnalyticsConnectorReportedState,
    pub value: ScalardbAnalyticsConnectorString,
    pub missing_reason: ScalardbAnalyticsConnectorString,
}

/// The kind of an Analytics type.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorAnalyticsTypeKind(pub u32);

impl ScalardbAnalyticsConnectorAnalyticsTypeKind {
    pub const BOOLEAN: Self = Self(1);
    pub const BYTE: Self = Self(2);
    pub const SMALL_INT: Self = Self(3);
    pub const INT: Self = Self(4);
    pub const BIG_INT: Self = Self(5);
    pub const FLOAT: Self = Self(6);
    pub const DOUBLE: Self = Self(7);
    pub const DECIMAL: Self = Self(8);
    pub const TEXT: Self = Self(9);
    pub const BLOB: Self = Self(10);
    pub const DATE: Self = Self(11);
    pub const TIME: Self = Self(12);
    pub const TIMESTAMP: Self = Self(13);
    pub const TIMESTAMP_TZ: Self = Self(14);
    pub const DURATION: Self = Self(15);
    pub const INTERVAL: Self = Self(16);
    pub const ARRAY: Self = Self(17);
    pub const MAP: Self = Self(18);
    pub const STRUCT: Self = Self(19);
    pub const UUID: Self = Self(20);
    pub const JSON: Self = Self(21);
    pub const ENUM: Self = Self(22);
    pub const YEAR: Self = Self(23);
    pub const TIME_WITH_TIME_ZONE: Self = Self(24);
}

/// An Analytics type, as the Analytics type model defines it.
///
/// The parameters of its kind are set: precision and scale for `DECIMAL`, the
/// element for `ARRAY`, the key and value for `MAP`, the fields for `STRUCT`,
/// and the members for `ENUM`. A type tree is acyclic.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorAnalyticsType {
    pub kind: ScalardbAnalyticsConnectorAnalyticsTypeKind,
    pub decimal_precision: u8,
    pub decimal_scale: i8,
    pub element_type: *const ScalardbAnalyticsConnectorAnalyticsType,
    pub element_nullable: ScalardbAnalyticsConnectorBool,
    pub key_type: *const ScalardbAnalyticsConnectorAnalyticsType,
    pub value_type: *const ScalardbAnalyticsConnectorAnalyticsType,
    pub value_nullable: ScalardbAnalyticsConnectorBool,
    pub fields: *const ScalardbAnalyticsConnectorAnalyticsField,
    pub field_count: usize,
    pub members: *const ScalardbAnalyticsConnectorString,
    pub member_count: usize,
}

/// A named, nullable field of a `STRUCT` Analytics type.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorAnalyticsField {
    pub name: ScalardbAnalyticsConnectorString,
    pub data_type: *const ScalardbAnalyticsConnectorAnalyticsType,
    pub nullable: ScalardbAnalyticsConnectorBool,
}

/// How a column's type was observed.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorColumnTypeState(pub u32);

impl ScalardbAnalyticsConnectorColumnTypeState {
    /// The Data Source type maps to an Analytics type.
    pub const MAPPED: Self = Self(1);
    /// The Data Source type was read but has no Analytics type.
    pub const UNSUPPORTED: Self = Self(2);
    /// The column's type could not be read.
    pub const UNREAD: Self = Self(3);
}

/// Why a column's Data Source type has no Analytics type.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorUnsupportedReason(pub u32);

impl ScalardbAnalyticsConnectorUnsupportedReason {
    pub const NOT_MAPPED: Self = Self(1);
    pub const MAPPING_NOT_IMPLEMENTED: Self = Self(2);
    pub const PARAMETER_OUT_OF_RANGE: Self = Self(3);
}

/// One column of a table.
///
/// `data_source_type` is set for `MAPPED` and `UNSUPPORTED`,
/// `analytics_type` for `MAPPED`, `unsupported_reason` for `UNSUPPORTED`, and
/// `unread_reason` for `UNREAD`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorColumn {
    pub name: ScalardbAnalyticsConnectorString,
    pub ordinal: u32,
    pub nullable: ScalardbAnalyticsConnectorBool,
    pub source_id: ScalardbAnalyticsConnectorReportedText,
    pub type_state: ScalardbAnalyticsConnectorColumnTypeState,
    pub data_source_type: ScalardbAnalyticsConnectorString,
    pub analytics_type: ScalardbAnalyticsConnectorAnalyticsType,
    pub unsupported_reason: ScalardbAnalyticsConnectorUnsupportedReason,
    pub unread_reason: ScalardbAnalyticsConnectorString,
}

/// The kind of a constraint.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorConstraintKind(pub u32);

impl ScalardbAnalyticsConnectorConstraintKind {
    pub const PRIMARY_KEY: Self = Self(1);
    pub const UNIQUE: Self = Self(2);
    pub const FOREIGN_KEY: Self = Self(3);
    pub const CHECK: Self = Self(4);
}

/// Whether uniqueness comes from a declared constraint or a unique index.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorUniqueKind(pub u32);

impl ScalardbAnalyticsConnectorUniqueKind {
    pub const CONSTRAINT: Self = Self(1);
    pub const INDEX: Self = Self(2);
}

/// One constraint of a table.
///
/// `columns` is set for every kind except `CHECK`; `unique_kind` and the
/// optional `condition` for `UNIQUE`; `referenced_table` and
/// `referenced_columns` for `FOREIGN_KEY`; `expression` for `CHECK`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorConstraint {
    pub name: ScalardbAnalyticsConnectorString,
    pub enforced: ScalardbAnalyticsConnectorBool,
    pub kind: ScalardbAnalyticsConnectorConstraintKind,
    pub columns: ScalardbAnalyticsConnectorNames,
    pub unique_kind: ScalardbAnalyticsConnectorUniqueKind,
    pub condition: ScalardbAnalyticsConnectorString,
    pub referenced_table: ScalardbAnalyticsConnectorNames,
    pub referenced_columns: ScalardbAnalyticsConnectorNames,
    pub expression: ScalardbAnalyticsConnectorString,
}

/// Source-independent metadata for one table that exists.
///
/// `other_kind` is set when `kind` is `OTHER`. The constraints are set when
/// `constraints_state` is `VALUE`, and `constraints_missing_reason` when it is
/// `MISSING`. Columns and constraints may be in any order on input.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorTableMetadata {
    pub table: ScalardbAnalyticsConnectorNames,
    pub kind: ScalardbAnalyticsConnectorTableKind,
    pub other_kind: ScalardbAnalyticsConnectorString,
    pub source_id: ScalardbAnalyticsConnectorReportedText,
    pub columns: *const ScalardbAnalyticsConnectorColumn,
    pub column_count: usize,
    /// True when the catalog may silently omit columns without privilege.
    pub columns_may_be_hidden: ScalardbAnalyticsConnectorBool,
    pub constraints_state: ScalardbAnalyticsConnectorReportedState,
    pub constraints_missing_reason: ScalardbAnalyticsConnectorString,
    pub constraints: *const ScalardbAnalyticsConnectorConstraint,
    pub constraint_count: usize,
}

/// A JSON document as text.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorJson {
    pub text: ScalardbAnalyticsConnectorString,
}

/// A connector's source metadata for one table: a JSON document whose shape
/// only that connector knows, and the name of the connector.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorSourceMetadata {
    pub connector: ScalardbAnalyticsConnectorString,
    pub document: ScalardbAnalyticsConnectorJson,
}

/// The state of one requested table.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalardbAnalyticsConnectorTableRecordState(pub u32);

impl ScalardbAnalyticsConnectorTableRecordState {
    /// The table was read and no item is missing.
    pub const COMPLETE: Self = Self(1);
    /// The table was read and some item is missing.
    pub const INCOMPLETE: Self = Self(2);
    /// The connector confirmed that the table does not exist.
    pub const DOES_NOT_EXIST: Self = Self(3);
    /// The table could not be read; this says nothing about whether it exists.
    pub const COULD_NOT_BE_READ: Self = Self(4);
}

/// The result for one requested table.
///
/// A table that was read has `table_metadata`, `source_metadata`, and
/// `read_at_microseconds`, the time the connector read its catalog in
/// microseconds since the Unix epoch. A table that could not be read has a
/// `reason`. Pointers that the state does not use are null.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorTableRecord {
    pub state: ScalardbAnalyticsConnectorTableRecordState,
    pub table: ScalardbAnalyticsConnectorNames,
    pub table_metadata: *const ScalardbAnalyticsConnectorTableMetadata,
    pub source_metadata: *const ScalardbAnalyticsConnectorSourceMetadata,
    pub read_at_microseconds: i64,
    pub reason: ScalardbAnalyticsConnectorString,
}

/// A named, versioned component; absent when `name` has a null `data`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorComponent {
    pub name: ScalardbAnalyticsConnectorString,
    pub version: ScalardbAnalyticsConnectorString,
}

/// How a table metadata request was retrieved. Comparisons never use it.
///
/// `adbc_driver` is absent with direct access.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorRetrieval {
    pub connector: ScalardbAnalyticsConnectorComponent,
    pub adbc_driver: ScalardbAnalyticsConnectorComponent,
    pub library_version: ScalardbAnalyticsConnectorString,
}

/// The result of a table metadata request: one record per requested table,
/// in request order. Freed with
/// `scalardb_analytics_connector_table_metadata_result_free`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorTableMetadataResult {
    pub records: *const ScalardbAnalyticsConnectorTableRecord,
    pub record_count: usize,
    pub retrieval: ScalardbAnalyticsConnectorRetrieval,
}

const ABSENT_NAMES: ScalardbAnalyticsConnectorNames = ScalardbAnalyticsConnectorNames {
    names: std::ptr::null(),
    name_count: 0,
};

const ABSENT: ScalardbAnalyticsConnectorString = ScalardbAnalyticsConnectorString::ABSENT;

// Writing outputs.

impl Arena {
    pub(super) fn names(&mut self, names: &[String]) -> ScalardbAnalyticsConnectorNames {
        let views = names.iter().map(|name| self.text(name)).collect();
        let (names, name_count) = self.array(views);
        ScalardbAnalyticsConnectorNames { names, name_count }
    }

    pub(super) fn namespace_list(
        &mut self,
        entries: &[Listed<NamespacePath>],
    ) -> ScalardbAnalyticsConnectorNamespaceList {
        let views = entries
            .iter()
            .map(|entry| match entry {
                Listed::Item(namespace) => ScalardbAnalyticsConnectorNamespaceEntry {
                    state: ScalardbAnalyticsConnectorListedState::ITEM,
                    namespace: self.names(&namespace.names),
                    scope: ABSENT_NAMES,
                    reason: ABSENT,
                },
                Listed::Unreadable { scope, reason } => ScalardbAnalyticsConnectorNamespaceEntry {
                    state: ScalardbAnalyticsConnectorListedState::UNREADABLE,
                    namespace: ABSENT_NAMES,
                    scope: self.names(&scope.names),
                    reason: self.text(reason),
                },
            })
            .collect();
        let (entries, entry_count) = self.array(views);
        ScalardbAnalyticsConnectorNamespaceList {
            entries,
            entry_count,
        }
    }

    pub(super) fn table_list(
        &mut self,
        entries: &[Listed<TableEntry>],
    ) -> ScalardbAnalyticsConnectorTableList {
        let views = entries
            .iter()
            .map(|entry| match entry {
                Listed::Item(entry) => {
                    let (kind, other_kind) = self.table_kind(&entry.kind);
                    ScalardbAnalyticsConnectorTableEntry {
                        state: ScalardbAnalyticsConnectorListedState::ITEM,
                        table: self.names(entry.table.names()),
                        kind,
                        other_kind,
                        scope: ABSENT_NAMES,
                        reason: ABSENT,
                    }
                }
                Listed::Unreadable { scope, reason } => ScalardbAnalyticsConnectorTableEntry {
                    state: ScalardbAnalyticsConnectorListedState::UNREADABLE,
                    table: ABSENT_NAMES,
                    kind: ScalardbAnalyticsConnectorTableKind(0),
                    other_kind: ABSENT,
                    scope: self.names(&scope.names),
                    reason: self.text(reason),
                },
            })
            .collect();
        let (entries, entry_count) = self.array(views);
        ScalardbAnalyticsConnectorTableList {
            entries,
            entry_count,
        }
    }

    fn table_kind(
        &mut self,
        kind: &TableKind,
    ) -> (
        ScalardbAnalyticsConnectorTableKind,
        ScalardbAnalyticsConnectorString,
    ) {
        match kind {
            TableKind::Table => (ScalardbAnalyticsConnectorTableKind::TABLE, ABSENT),
            TableKind::View => (ScalardbAnalyticsConnectorTableKind::VIEW, ABSENT),
            TableKind::External => (ScalardbAnalyticsConnectorTableKind::EXTERNAL, ABSENT),
            TableKind::Other(term) => (ScalardbAnalyticsConnectorTableKind::OTHER, self.text(term)),
        }
    }

    pub(super) fn table_metadata_result(
        &mut self,
        result: &TableMetadataResult,
    ) -> ScalardbAnalyticsConnectorTableMetadataResult {
        let views = result
            .records
            .iter()
            .map(|record| self.table_record(record))
            .collect();
        let (records, record_count) = self.array(views);
        let retrieval = self.retrieval(&result.retrieval);
        ScalardbAnalyticsConnectorTableMetadataResult {
            records,
            record_count,
            retrieval,
        }
    }

    fn table_record(
        &mut self,
        record: &TableRecord<TableRead>,
    ) -> ScalardbAnalyticsConnectorTableRecord {
        match record {
            TableRecord::Read { metadata, read_at } => {
                let state = if metadata.table_metadata.is_complete() {
                    ScalardbAnalyticsConnectorTableRecordState::COMPLETE
                } else {
                    ScalardbAnalyticsConnectorTableRecordState::INCOMPLETE
                };
                let table_metadata = self.table_metadata(&metadata.table_metadata);
                let source_metadata = self.source_metadata(&metadata.source_metadata);
                ScalardbAnalyticsConnectorTableRecord {
                    state,
                    table: self.names(metadata.table_metadata.table().names()),
                    table_metadata: self.one(table_metadata),
                    source_metadata: self.one(source_metadata),
                    read_at_microseconds: microseconds_since_epoch(*read_at),
                    reason: ABSENT,
                }
            }
            TableRecord::NotFound { table } => ScalardbAnalyticsConnectorTableRecord {
                state: ScalardbAnalyticsConnectorTableRecordState::DOES_NOT_EXIST,
                table: self.names(table.names()),
                table_metadata: std::ptr::null(),
                source_metadata: std::ptr::null(),
                read_at_microseconds: 0,
                reason: ABSENT,
            },
            TableRecord::Unavailable { table, reason } => ScalardbAnalyticsConnectorTableRecord {
                state: ScalardbAnalyticsConnectorTableRecordState::COULD_NOT_BE_READ,
                table: self.names(table.names()),
                table_metadata: std::ptr::null(),
                source_metadata: std::ptr::null(),
                read_at_microseconds: 0,
                reason: self.text(reason),
            },
        }
    }

    fn retrieval(&mut self, retrieval: &Retrieval) -> ScalardbAnalyticsConnectorRetrieval {
        ScalardbAnalyticsConnectorRetrieval {
            connector: self.component(Some(&retrieval.connector)),
            adbc_driver: self.component(retrieval.adbc_driver.as_ref()),
            library_version: self.text(&retrieval.library_version),
        }
    }

    fn component(&mut self, component: Option<&Component>) -> ScalardbAnalyticsConnectorComponent {
        ScalardbAnalyticsConnectorComponent {
            name: self.optional_text(component.map(|component| component.name.as_str())),
            version: self.optional_text(component.map(|component| component.version.as_str())),
        }
    }

    pub(super) fn source_metadata(
        &mut self,
        metadata: &SourceMetadata,
    ) -> ScalardbAnalyticsConnectorSourceMetadata {
        ScalardbAnalyticsConnectorSourceMetadata {
            connector: self.text(&metadata.connector),
            document: ScalardbAnalyticsConnectorJson {
                text: self.text(&metadata.document),
            },
        }
    }

    pub(super) fn table_metadata(
        &mut self,
        metadata: &TableMetadata,
    ) -> ScalardbAnalyticsConnectorTableMetadata {
        let (kind, other_kind) = self.table_kind(metadata.kind());
        let source_id = self.reported_text(metadata.source_id());
        let columns = metadata
            .columns()
            .iter()
            .map(|column| self.column(column))
            .collect();
        let (columns, column_count) = self.array(columns);
        let (constraints_state, constraints_missing_reason, constraints) =
            match metadata.constraints() {
                Reported::Value(constraints) => (
                    ScalardbAnalyticsConnectorReportedState::VALUE,
                    ABSENT,
                    constraints
                        .iter()
                        .map(|constraint| self.constraint(constraint))
                        .collect(),
                ),
                Reported::Missing { reason } => (
                    ScalardbAnalyticsConnectorReportedState::MISSING,
                    self.text(reason),
                    Vec::new(),
                ),
                Reported::NotOffered => (
                    ScalardbAnalyticsConnectorReportedState::NOT_OFFERED,
                    ABSENT,
                    Vec::new(),
                ),
            };
        let (constraints, constraint_count) = self.array(constraints);
        ScalardbAnalyticsConnectorTableMetadata {
            table: self.names(metadata.table().names()),
            kind,
            other_kind,
            source_id,
            columns,
            column_count,
            columns_may_be_hidden: metadata.columns_may_be_hidden().into(),
            constraints_state,
            constraints_missing_reason,
            constraints,
            constraint_count,
        }
    }

    fn reported_text(&mut self, item: &Reported<String>) -> ScalardbAnalyticsConnectorReportedText {
        match item {
            Reported::Value(value) => ScalardbAnalyticsConnectorReportedText {
                state: ScalardbAnalyticsConnectorReportedState::VALUE,
                value: self.text(value),
                missing_reason: ABSENT,
            },
            Reported::Missing { reason } => ScalardbAnalyticsConnectorReportedText {
                state: ScalardbAnalyticsConnectorReportedState::MISSING,
                value: ABSENT,
                missing_reason: self.text(reason),
            },
            Reported::NotOffered => ScalardbAnalyticsConnectorReportedText {
                state: ScalardbAnalyticsConnectorReportedState::NOT_OFFERED,
                value: ABSENT,
                missing_reason: ABSENT,
            },
        }
    }

    pub(super) fn column(&mut self, column: &Column) -> ScalardbAnalyticsConnectorColumn {
        let mut view = ScalardbAnalyticsConnectorColumn {
            name: self.text(&column.name),
            ordinal: column.ordinal,
            nullable: column.nullable.into(),
            source_id: self.reported_text(&column.source_id),
            type_state: ScalardbAnalyticsConnectorColumnTypeState(0),
            data_source_type: ABSENT,
            analytics_type: NO_TYPE,
            unsupported_reason: ScalardbAnalyticsConnectorUnsupportedReason(0),
            unread_reason: ABSENT,
        };
        match &column.ty {
            ColumnType::Mapped {
                data_source_type,
                analytics_type,
            } => {
                view.type_state = ScalardbAnalyticsConnectorColumnTypeState::MAPPED;
                view.data_source_type = self.text(data_source_type);
                view.analytics_type = self.analytics_type(analytics_type);
            }
            ColumnType::Unsupported {
                data_source_type,
                reason,
            } => {
                view.type_state = ScalardbAnalyticsConnectorColumnTypeState::UNSUPPORTED;
                view.data_source_type = self.text(data_source_type);
                view.unsupported_reason = match reason {
                    UnsupportedReason::NotMapped => {
                        ScalardbAnalyticsConnectorUnsupportedReason::NOT_MAPPED
                    }
                    UnsupportedReason::MappingNotImplemented => {
                        ScalardbAnalyticsConnectorUnsupportedReason::MAPPING_NOT_IMPLEMENTED
                    }
                    UnsupportedReason::ParameterOutOfRange => {
                        ScalardbAnalyticsConnectorUnsupportedReason::PARAMETER_OUT_OF_RANGE
                    }
                };
            }
            ColumnType::Unread { reason } => {
                view.type_state = ScalardbAnalyticsConnectorColumnTypeState::UNREAD;
                view.unread_reason = self.text(reason);
            }
        }
        view
    }

    fn analytics_type(
        &mut self,
        analytics_type: &AnalyticsType,
    ) -> ScalardbAnalyticsConnectorAnalyticsType {
        use ScalardbAnalyticsConnectorAnalyticsTypeKind as Kind;
        let mut view = NO_TYPE;
        view.kind = match analytics_type {
            AnalyticsType::Boolean => Kind::BOOLEAN,
            AnalyticsType::Byte => Kind::BYTE,
            AnalyticsType::SmallInt => Kind::SMALL_INT,
            AnalyticsType::Int => Kind::INT,
            AnalyticsType::BigInt => Kind::BIG_INT,
            AnalyticsType::Float => Kind::FLOAT,
            AnalyticsType::Double => Kind::DOUBLE,
            AnalyticsType::Decimal(decimal) => {
                view.decimal_precision = decimal.precision();
                view.decimal_scale = decimal.scale();
                Kind::DECIMAL
            }
            AnalyticsType::Text => Kind::TEXT,
            AnalyticsType::Blob => Kind::BLOB,
            AnalyticsType::Date => Kind::DATE,
            AnalyticsType::Time => Kind::TIME,
            AnalyticsType::Timestamp => Kind::TIMESTAMP,
            AnalyticsType::TimestampTz => Kind::TIMESTAMP_TZ,
            AnalyticsType::Duration => Kind::DURATION,
            AnalyticsType::Interval => Kind::INTERVAL,
            AnalyticsType::Array(array) => {
                let element = self.analytics_type(array.element_type());
                view.element_type = self.one(element);
                view.element_nullable = array.is_element_nullable().into();
                Kind::ARRAY
            }
            AnalyticsType::Map(map) => {
                let key = self.analytics_type(map.key_type());
                let value = self.analytics_type(map.value_type());
                view.key_type = self.one(key);
                view.value_type = self.one(value);
                view.value_nullable = map.is_value_nullable().into();
                Kind::MAP
            }
            AnalyticsType::Struct(structure) => {
                let fields = structure
                    .fields()
                    .iter()
                    .map(|field| {
                        let data_type = self.analytics_type(field.data_type());
                        ScalardbAnalyticsConnectorAnalyticsField {
                            name: self.text(field.name()),
                            data_type: self.one(data_type),
                            nullable: field.is_nullable().into(),
                        }
                    })
                    .collect();
                (view.fields, view.field_count) = self.array(fields);
                Kind::STRUCT
            }
            AnalyticsType::Uuid => Kind::UUID,
            AnalyticsType::Json => Kind::JSON,
            AnalyticsType::Enum(enumeration) => {
                let members = enumeration
                    .members()
                    .iter()
                    .map(|member| self.text(member))
                    .collect();
                (view.members, view.member_count) = self.array(members);
                Kind::ENUM
            }
            AnalyticsType::Year => Kind::YEAR,
            AnalyticsType::TimeWithTimeZone => Kind::TIME_WITH_TIME_ZONE,
        };
        view
    }

    pub(super) fn constraint(
        &mut self,
        constraint: &Constraint,
    ) -> ScalardbAnalyticsConnectorConstraint {
        let mut view = ScalardbAnalyticsConnectorConstraint {
            name: self.text(&constraint.name),
            enforced: constraint.enforced.into(),
            kind: ScalardbAnalyticsConnectorConstraintKind(0),
            columns: ABSENT_NAMES,
            unique_kind: ScalardbAnalyticsConnectorUniqueKind(0),
            condition: ABSENT,
            referenced_table: ABSENT_NAMES,
            referenced_columns: ABSENT_NAMES,
            expression: ABSENT,
        };
        match &constraint.kind {
            ConstraintKind::PrimaryKey { columns } => {
                view.kind = ScalardbAnalyticsConnectorConstraintKind::PRIMARY_KEY;
                view.columns = self.names(columns);
            }
            ConstraintKind::Unique {
                columns,
                kind,
                condition,
            } => {
                view.kind = ScalardbAnalyticsConnectorConstraintKind::UNIQUE;
                view.columns = self.names(columns);
                view.unique_kind = match kind {
                    UniqueKind::Constraint => ScalardbAnalyticsConnectorUniqueKind::CONSTRAINT,
                    UniqueKind::Index => ScalardbAnalyticsConnectorUniqueKind::INDEX,
                };
                view.condition = self.optional_text(condition.as_deref());
            }
            ConstraintKind::ForeignKey {
                columns,
                referenced_table,
                referenced_columns,
            } => {
                view.kind = ScalardbAnalyticsConnectorConstraintKind::FOREIGN_KEY;
                view.columns = self.names(columns);
                view.referenced_table = self.names(referenced_table.names());
                view.referenced_columns = self.names(referenced_columns);
            }
            ConstraintKind::Check { expression } => {
                view.kind = ScalardbAnalyticsConnectorConstraintKind::CHECK;
                view.expression = self.text(expression);
            }
        }
        view
    }
}

const NO_TYPE: ScalardbAnalyticsConnectorAnalyticsType = ScalardbAnalyticsConnectorAnalyticsType {
    kind: ScalardbAnalyticsConnectorAnalyticsTypeKind(0),
    decimal_precision: 0,
    decimal_scale: 0,
    element_type: std::ptr::null(),
    element_nullable: ScalardbAnalyticsConnectorBool::FALSE,
    key_type: std::ptr::null(),
    value_type: std::ptr::null(),
    value_nullable: ScalardbAnalyticsConnectorBool::FALSE,
    fields: std::ptr::null(),
    field_count: 0,
    members: std::ptr::null(),
    member_count: 0,
};

fn microseconds_since_epoch(time: SystemTime) -> i64 {
    let micros = |duration: Duration| i64::try_from(duration.as_micros()).unwrap_or(i64::MAX);
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => micros(after),
        Err(before) => -micros(before.duration()),
    }
}

// Reading inputs.
//
// Every function below requires that the pointers inside the structure it
// reads, and inside everything those point to, are null or point to as many
// readable values as their counts state.

/// Reads required text.
///
/// # Safety
///
/// See the section comment.
pub(super) unsafe fn read_string(
    text: ScalardbAnalyticsConnectorString,
    name: &str,
) -> Result<String, ConnectorError> {
    // SAFETY: forwarded from the caller.
    unsafe { read_optional_string(text, name) }?
        .ok_or_else(|| invalid_request(&format!("{name} is absent")))
}

/// Reads text that may be absent, which a null `data` marks.
///
/// # Safety
///
/// See the section comment.
unsafe fn read_optional_string(
    text: ScalardbAnalyticsConnectorString,
    name: &str,
) -> Result<Option<String>, ConnectorError> {
    if text.data.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { std::slice::from_raw_parts(text.data.cast::<u8>(), text.length) };
    String::from_utf8(bytes.to_vec())
        .map(Some)
        .map_err(|_| invalid_request(&format!("{name} is not UTF-8")))
}

/// Reads a sequence of names.
///
/// # Safety
///
/// See the section comment.
pub(super) unsafe fn read_names(
    names: &ScalardbAnalyticsConnectorNames,
    name: &str,
) -> Result<Vec<String>, ConnectorError> {
    // SAFETY: forwarded from the caller.
    unsafe { read_slice(names.names, names.name_count, name) }?
        .iter()
        // SAFETY: forwarded from the caller.
        .map(|&text| unsafe { read_string(text, name) })
        .collect()
}

/// Reads a table name path.
///
/// # Safety
///
/// See the section comment.
pub(super) unsafe fn read_table_reference(
    names: &ScalardbAnalyticsConnectorNames,
    name: &str,
) -> Result<TableReference, ConnectorError> {
    // SAFETY: forwarded from the caller.
    let names = unsafe { read_names(names, name) }?;
    TableReference::new(names).map_err(|error| invalid_request(&error.to_string()))
}
