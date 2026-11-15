//! The two layers of table metadata and the states of one requested table.

use std::time::SystemTime;

use analytics_types::AnalyticsType;

use crate::RequestError;

/// The result for one table named in a table metadata request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableRecord<T> {
    /// The table exists and was read, completely or not.
    Read {
        metadata: T,
        /// When the connector read this table's catalog. It is retrieval info,
        /// not content: it changes without a change at the source, so
        /// comparisons do not use it.
        read_at: SystemTime,
    },
    /// The connector confirmed that the table does not exist.
    DoesNotExist { table: TableReference },
    /// The table exists, but the credential lacks the privilege to read it.
    PermissionDenied {
        table: TableReference,
        reason: String,
    },
    /// The table cannot be found, and the connector cannot tell whether it
    /// does not exist or is hidden: the catalog silently omits tables without
    /// privilege.
    NotFound {
        table: TableReference,
        reason: String,
    },
}

/// An unresolved non-empty table name path used by metadata requests.
///
/// For sources with catalogs, the first name is the catalog. It does not prove
/// membership in an open Data Source or registered schema and is distinct from
/// a Substrait table binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TableReference {
    names: Vec<String>,
}

impl TableReference {
    /// Creates an unresolved identifier from non-empty name components.
    pub fn new(names: Vec<String>) -> Result<Self, RequestError> {
        if names.is_empty() || names.iter().any(String::is_empty) {
            return Err(RequestError::invalid(
                "a table reference must contain only non-empty names",
            ));
        }
        Ok(Self { names })
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// Source-independent metadata for one table that exists, as the Physical
/// Catalog registers it.
///
/// Construction fixes the order of columns and constraints, so equal source
/// state gives equal values. Completeness is derived from the values rather
/// than stored, so it cannot disagree with them. The fields are private to
/// keep that order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableMetadata {
    table: TableReference,
    kind: TableKind,
    source_id: Reported<String>,
    columns: Vec<Column>,
    columns_may_be_hidden: bool,
    constraints: Reported<Vec<Constraint>>,
}

impl TableMetadata {
    /// Creates table metadata, ordering columns by ordinal and constraints by
    /// kind, name, and columns.
    ///
    /// `columns_may_be_hidden` is true when the catalog silently omits columns
    /// without privilege and the read could not rule that out.
    pub fn new(
        table: TableReference,
        kind: TableKind,
        source_id: Reported<String>,
        mut columns: Vec<Column>,
        columns_may_be_hidden: bool,
        mut constraints: Reported<Vec<Constraint>>,
    ) -> Self {
        columns.sort_by_key(|column| column.ordinal);
        if let Reported::Value(constraints) = &mut constraints {
            constraints.sort_by(|left, right| left.order_key().cmp(&right.order_key()));
        }
        Self {
            table,
            kind,
            source_id,
            columns,
            columns_may_be_hidden,
            constraints,
        }
    }

    /// Whether no item is missing; items the connector never offers do not count.
    pub fn is_complete(&self) -> bool {
        !self.columns_may_be_hidden
            && !self.source_id.is_missing()
            && !self.constraints.is_missing()
            && self
                .columns
                .iter()
                .all(|column| !column.source_id.is_missing() && !column.ty.is_unread())
    }

    pub fn table(&self) -> &TableReference {
        &self.table
    }

    pub fn kind(&self) -> &TableKind {
        &self.kind
    }

    pub fn source_id(&self) -> &Reported<String> {
        &self.source_id
    }

    /// Columns in ordinal order.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn columns_may_be_hidden(&self) -> bool {
        self.columns_may_be_hidden
    }

    /// Constraints ordered by kind, name, and columns.
    pub fn constraints(&self) -> &Reported<Vec<Constraint>> {
        &self.constraints
    }
}

/// The kind of table the source distinguishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableKind {
    Table,
    View,
    External,
    /// Another kind the source distinguishes, in the source's own term.
    Other(String),
}

/// An item that the connector read, could not read, or never offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reported<T> {
    Value(T),
    /// The item exists in principle but could not be read; the table is incomplete.
    Missing {
        reason: String,
    },
    /// The connector never returns this item for its Connector, such as
    /// constraints for Parquet; the table stays complete.
    NotOffered,
}

impl<T> Reported<T> {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing { .. })
    }
}

/// One column the source declares; unsupported columns are never dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// The name as the source spells it, without case folding.
    pub name: String,
    /// The source's column order, without gaps.
    pub ordinal: u32,
    pub nullable: bool,
    pub source_id: Reported<String>,
    pub ty: ColumnType,
}

/// The column's Data Source type and the Analytics type the connector derived.
///
/// An Analytics type cannot exist without the Data Source type it was mapped from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnType {
    Mapped {
        data_source_type: String,
        analytics_type: AnalyticsType,
    },
    Unsupported {
        data_source_type: String,
        reason: UnsupportedReason,
    },
    /// The Data Source type could not be read; the table is incomplete.
    Unread { reason: String },
}

impl ColumnType {
    fn is_unread(&self) -> bool {
        matches!(self, Self::Unread { .. })
    }
}

/// Why a column has no Analytics type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// The type mapping states that the type is not mapped.
    NotMapped,
    /// The type mapping maps the type, but this connector version does not
    /// implement that mapping rule: its read path cannot produce the Analytics
    /// values without changing them.
    ///
    /// The cause may be an ADBC driver that converts or cannot return the type,
    /// a reader crate that cannot read it, or a missing value-preserving
    /// conversion in the connector itself. A later connector version may map it.
    MappingNotImplemented,
    /// A type parameter, such as precision or unit, exceeds the mappable range.
    ParameterOutOfRange,
}

/// One constraint the source declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constraint {
    pub name: String,
    /// False for a declared-only constraint that the source does not enforce.
    pub enforced: bool,
    pub kind: ConstraintKind,
}

impl Constraint {
    /// The deterministic order: kind, then name, then columns.
    fn order_key(&self) -> (u8, &str, &[String]) {
        let (rank, columns) = match &self.kind {
            ConstraintKind::PrimaryKey { columns } => (0, columns.as_slice()),
            ConstraintKind::Unique { columns, .. } => (1, columns.as_slice()),
            ConstraintKind::ForeignKey { columns, .. } => (2, columns.as_slice()),
            ConstraintKind::Check { .. } => (3, [].as_slice()),
        };
        (rank, self.name.as_str(), columns)
    }
}

/// The kinds of constraints table metadata returns. NOT NULL is column nullability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstraintKind {
    PrimaryKey {
        columns: Vec<String>,
    },
    Unique {
        columns: Vec<String>,
        kind: UniqueKind,
        /// A partial index's condition in the source's notation, uninterpreted.
        condition: Option<String>,
    },
    ForeignKey {
        columns: Vec<String>,
        /// The referenced table, within the same Data Source.
        referenced_table: TableReference,
        referenced_columns: Vec<String>,
    },
    Check {
        /// The expression in the source's notation, uninterpreted.
        expression: String,
    },
}

/// Whether uniqueness comes from a constraint or from a unique index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UniqueKind {
    Constraint,
    Index,
}
