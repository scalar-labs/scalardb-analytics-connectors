//! Shared ODBC access for Connectors.
//!
//! A Connector that reaches its Data Source through the ODBC driver manager
//! implements [`OdbcConnector`] and names [`Odbc`] as its access. [`Odbc`]
//! opens one ODBC connection per session from the connection string the
//! Connector builds; [`query`] runs a statement on a session's
//! [`OdbcConnection`], reads its result as Arrow through `arrow-odbc`, and
//! classifies ODBC diagnostics with the Connector's error handling. A
//! SQL-speaking Connector uses [`query`] for `connector-sql`'s
//! `SqlConnector::query`.
//!
//! The ODBC driver manager is a native library this crate links dynamically;
//! the source's ODBC driver is loaded by the driver manager at run time.

mod connection;
mod connector;
mod error;
mod query;

pub use connection::OdbcConnection;
pub use connector::{ConnectionString, Odbc, OdbcConnector, OdbcDataSource, OdbcLibrary};
pub use error::OdbcDiagnostic;
pub use query::query;
