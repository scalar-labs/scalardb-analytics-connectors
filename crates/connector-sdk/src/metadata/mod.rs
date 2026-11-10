//! Connector lifecycle and metadata retrieval contracts.
//!
//! Table metadata has two layers. A connector defines its own source-specific
//! table type, which the Semantic Catalog receives as a JSON document (source
//! metadata). From it, the connector derives the source-independent
//! [`TableMetadata`] that the Physical Catalog registers.

mod listing;
mod table;

pub use listing::{Listed, NameFilter, NamespacePath, TableEntry};
pub use table::{
    Column, ColumnType, Constraint, ConstraintKind, Reported, SourceTableMetadata, TableKind,
    TableMetadata, TableRecord, TableReference, UniqueKind, UnsupportedReason,
};

use crate::{ConnectorError, ConnectorExecutor, ConnectorPlanner, ExecutionOutput};

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

/// The name and version of one component that produced table metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub version: String,
}

/// The connector and ADBC driver behind a session, recorded in retrieval info.
///
/// The library version is added by the connector API, which the SDK cannot see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Implementation {
    /// The connector and its version, which also identifies its type mapping
    /// and built-in reader crates.
    pub connector: Component,
    /// The ADBC driver the session uses; absent with direct access.
    pub adbc_driver: Option<Component>,
}

/// Process-level connector factory.
pub trait ConnectorLibrary: Send + Sync {
    /// Process-level settings used to open a Data Source handle.
    type DataSourceConfig;
    /// Long-lived Data Source handle created by this library.
    type DataSource: DataSource;

    /// Opens a configured Data Source handle.
    fn open_data_source(
        &self,
        config: Self::DataSourceConfig,
    ) -> Result<Self::DataSource, ConnectorError>;
}

/// Long-lived Data Source handle that can create isolated sessions.
pub trait DataSource: Send + Sync {
    /// Settings whose lifetime and isolation belong to one session.
    type SessionConfig;
    /// Session implementation created by this handle.
    type Session: DataSourceSession;

    /// Opens an isolated session with catalog, planner, and executor state.
    fn open_session(&self, config: Self::SessionConfig) -> Result<Self::Session, ConnectorError>;
}

/// Session-scoped catalog, planning, and execution capabilities.
pub trait DataSourceSession: Send + Sync {
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
    /// Returns the connector and ADBC driver that this session reads through.
    fn implementation(&self) -> Implementation;
}
