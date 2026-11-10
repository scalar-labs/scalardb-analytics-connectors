//! Metadata retrieval contracts.
//!
//! Table metadata has two layers. A connector defines its own source-specific
//! table type, which the Semantic Catalog receives as a JSON document (source
//! metadata). From it, the connector derives the source-independent
//! [`TableMetadata`] that the Physical Catalog registers.

mod comparison;
mod listing;
mod table;

pub use comparison::{
    ColumnAttribute, ColumnChange, Completeness, ConstraintChange, ConstraintChanges, Difference,
    Side, TableAttribute, TableChanges, TableComparison, TableState,
};
pub use listing::{Listed, NameFilter, NamespacePath, TableEntry};
pub use table::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, SourceTableMetadata, TableKind,
    TableMetadata, TableRecord, TableReference, UniqueKind, UnsupportedReason,
};

use crate::{ConnectorError, ExecutionOutput};

/// Reads one Data Source's catalog for the three metadata requests.
///
/// Each function starts one catalog read and returns its item stream with the
/// means of stopping it. Wrap the call with [`crate::start_source_read`] to
/// apply cancellation and the deadline.
pub trait CatalogReader: Send + Sync {
    /// Connector-specific metadata for one table, such as PostgreSQL `pg_catalog` facts.
    type Table: SourceTableMetadata;

    /// Lists namespaces whose names exactly match `filter`, or all of them.
    fn list_namespaces(
        &self,
        filter: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<NamespacePath>>, ConnectorError>;

    /// Lists tables across the matching namespaces whose names match `tables`.
    fn list_tables(
        &self,
        namespaces: &NameFilter,
        tables: &NameFilter,
    ) -> Result<ExecutionOutput<Listed<TableEntry>>, ConnectorError>;

    /// Reads one record per requested table, including tables that do not
    /// exist or could not be read.
    fn read_tables(
        &self,
        tables: &[TableReference],
    ) -> Result<ExecutionOutput<TableRecord<Self::Table>>, ConnectorError>;
}
