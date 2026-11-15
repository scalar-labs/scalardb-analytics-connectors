//! Metadata retrieval contracts.
//!
//! Table metadata has two layers. A connector defines its own source-specific
//! table type, which the Semantic Catalog receives as a JSON document (source
//! metadata). From it and the Data Source's settings, the connector derives
//! the source-independent [`TableMetadata`] that the Physical Catalog
//! registers ([`crate::Catalog::derive_table_metadata`]).

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
    Column, ColumnType, Constraint, ConstraintKind, Reported, TableKind, TableMetadata,
    TableRecord, TableReference, UniqueKind, UnsupportedReason,
};
