//! Shared ADBC access behavior for Connectors.

#![forbid(unsafe_code)]

mod error;
mod query;

pub use error::{AdbcDetail, AdbcDetails, AdbcDiagnostic, AdbcStatus, classify_error};
pub use query::{AdbcQueryCancelHandle, AdbcQueryStream, query};
