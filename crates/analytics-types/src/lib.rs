//! Data Source-independent Analytics schema types and their Arrow representation.
//!
//! Analytics types form one federation schema type system. Data Source types,
//! Arrow storage and extension types, and Query Engine types are boundary
//! representations connected to that model by explicit mappings.

#![forbid(unsafe_code)]

mod arrow;
mod error;
mod model;

pub use error::TypeError;
pub use model::{
    AnalyticsField, AnalyticsSchema, AnalyticsType, ArrayType, DecimalType, EnumType,
    MAX_DECIMAL_PRECISION, MapType, StructType,
};

pub use arrow_schema;
