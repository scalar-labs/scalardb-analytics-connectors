//! Connector lifecycle: the process-level library, Data Sources, and sessions.
//!
//! The connector API opens every unit through these traits. A connector states
//! what it accepts in its [`ConnectorDescriptor`]; the API validates caller
//! options against it before the connector sees them.

mod descriptor;
mod options;

pub use descriptor::{ConnectorDescriptor, DataSourceAccess, OptionDescriptor, OptionValueType};
pub use options::{ConnectionOptions, OptionValue, SecretString};

use std::{path::PathBuf, time::Duration};

use crate::{CatalogReader, ConnectorError, ConnectorExecutor, ConnectorPlanner};

/// Process-wide settings received when the connector API library is initialized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibrarySettings {
    /// Directory from which ADBC drivers are loaded; absent when none is configured.
    pub adbc_driver_directory: Option<PathBuf>,
}

/// The name and version of one component that produced table metadata.
///
/// Retrieval info names the connector from its [`ConnectorDescriptor`], the
/// ADBC driver from [`DataSourceSession::adbc_driver`], and the library
/// version from the connector API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub version: String,
}

/// Process-level connector factory, created once per library initialization.
pub trait ConnectorLibrary: Send + Sync + Sized + 'static {
    /// Long-lived Data Source handle created by this library.
    type DataSource: DataSource;

    /// Describes this connector for static information and option validation.
    ///
    /// It is available before the library is initialized.
    fn descriptor() -> ConnectorDescriptor;

    /// Creates the connector's process-level state from the library settings.
    fn new(settings: &LibrarySettings) -> Result<Self, ConnectorError>;

    /// Opens a Data Source of the named Connector.
    ///
    /// `connector` is one of [`ConnectorDescriptor::connectors`]. `options` were
    /// already validated against the descriptor. `connection_timeout` limits
    /// connecting while this Data Source and its sessions open; `None` means no
    /// limit.
    fn open_data_source(
        &self,
        connector: &str,
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Self::DataSource, ConnectorError>;
}

/// Long-lived Data Source handle that can create isolated sessions.
pub trait DataSource: Send + Sync + 'static {
    /// Session implementation created by this handle.
    type Session: DataSourceSession;

    /// Opens an isolated session with catalog, planner, and executor state.
    fn open_session(&self) -> Result<Self::Session, ConnectorError>;
}

/// Session-scoped catalog, planning, and execution capabilities.
pub trait DataSourceSession: Send + Sync + 'static {
    /// Connector-owned instructions shared by this session's planner and executor.
    type Query;
    /// Catalog implementation bound to this session.
    type Catalog: CatalogReader;
    /// Pushdown planner bound to this session.
    type Planner: ConnectorPlanner<Query = Self::Query>;
    /// Remote-query executor bound to this session.
    type Executor: ConnectorExecutor<Query = Self::Query>;

    /// Returns the session-scoped catalog reader.
    fn catalog(&self) -> &Self::Catalog;
    /// Returns the session-scoped pushdown planner.
    fn planner(&self) -> &Self::Planner;
    /// Returns the session-scoped remote-query executor.
    fn executor(&self) -> &Self::Executor;
    /// Returns the ADBC driver this session reads through; `None` with direct access.
    fn adbc_driver(&self) -> Option<Component>;
}
