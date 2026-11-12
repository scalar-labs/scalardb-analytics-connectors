//! Shared ADBC access for Connectors.
//!
//! A Connector that reaches its Data Source through an ADBC driver implements
//! [`AdbcConnector`] and names [`Adbc`] as its access. [`Adbc`] loads the
//! driver, opens databases and connections, checks the driver version, and
//! builds errors from the driver's diagnostics; [`query`] runs a statement on a
//! session's [`AdbcConnection`] with the Connector's error handling.

#![forbid(unsafe_code)]

mod connection;
mod connector;
mod data_source;
mod driver;
mod error;
mod query;

pub use connection::{AdbcConnection, AdbcSession};
pub use connector::{Adbc, AdbcConnector, AdbcLibrary};
pub use data_source::AdbcDatabase;
pub use driver::{DriverRequirement, DriverVersion};
pub use error::{AdbcDetail, AdbcDetails, AdbcDiagnostic, AdbcStatus};
pub use query::{AdbcQueryCancelHandle, AdbcQueryStream, query};
