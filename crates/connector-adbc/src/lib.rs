//! Shared ADBC access behavior for Connectors.

#![forbid(unsafe_code)]

mod query;

pub use query::{AdbcQueryStream, query};
