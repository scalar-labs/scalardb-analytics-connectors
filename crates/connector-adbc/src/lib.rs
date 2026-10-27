//! Shared ADBC access behavior for source-specific connectors.

#![forbid(unsafe_code)]

mod query;

pub use query::{AdbcQueryStream, query};
