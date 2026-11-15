//! Metadata retrieval contracts.
//!
//! Table metadata has two layers. A connector defines its own source-specific
//! table type, which the Semantic Catalog receives as a JSON document (source
//! metadata). From it, the connector derives the source-independent
//! [`TableMetadata`] that the Physical Catalog registers.

mod comparison;
mod listing;
mod schema_check;
mod table;

pub(crate) use comparison::compare_source_metadata;
pub use comparison::{
    Change, ColumnAttribute, ConstraintAttribute, JsonDifference, TableAttribute,
    TableMetadataDifferences,
};
pub use listing::{Listed, NameFilter, NamespacePath, TableEntry};
pub use schema_check::check_schema;
pub use table::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, SourceTableMetadata, TableKind,
    TableMetadata, TableRecord, TableReference, UniqueKind, UnsupportedReason,
};
