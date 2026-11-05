//! Data Source-independent metadata and connector lifecycle contracts.

use analytics_types::{AnalyticsSchema, AnalyticsType};
use sha2::{Digest, Sha256};

use crate::{ConnectorError, ConnectorExecutor, ConnectorPlanner, RequestError};

/// Completeness of a metadata lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataState<T> {
    /// The requested metadata is available without known omissions.
    Complete(T),
    /// Usable metadata is available, with explicitly reported omissions.
    Incomplete { value: T, missing: Vec<MetadataGap> },
    /// The requested object does not exist in the observed catalog state.
    NotFound,
    /// The lookup could not determine catalog state.
    Unavailable { reason: String },
}

/// A part of table metadata that the Data Source could not supply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataGap {
    /// Metadata field that could not be supplied.
    pub field: String,
    /// Connector-provided explanation of the omission.
    pub reason: String,
}

impl MetadataGap {
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

/// Stable digest of a connector-defined canonical metadata representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataDigest([u8; 32]);

impl MetadataDigest {
    /// Hashes a Connector-defined canonical metadata encoding.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Provenance attached to one table metadata result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataProvenance {
    /// Data Source product observed during metadata retrieval.
    pub data_source_product: String,
    /// Data Source version observed during metadata retrieval.
    pub data_source_version: String,
    /// Optional Connector-defined identity of the observed catalog state.
    pub catalog_snapshot: Option<String>,
}

impl MetadataProvenance {
    pub fn new(
        data_source_product: impl Into<String>,
        data_source_version: impl Into<String>,
        catalog_snapshot: Option<String>,
    ) -> Self {
        Self {
            data_source_product: data_source_product.into(),
            data_source_version: data_source_version.into(),
            catalog_snapshot,
        }
    }
}

/// An unresolved non-empty table name path used by metadata requests.
///
/// It does not prove membership in an open Data Source or registered schema
/// and is distinct from a Substrait table binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableReference {
    names: Vec<String>,
}

impl TableReference {
    /// Creates an unresolved identifier from non-empty name components.
    pub fn new(names: Vec<String>) -> Result<Self, RequestError> {
        if names.is_empty() || names.iter().any(String::is_empty) {
            return Err(RequestError::invalid(
                "a table reference must contain only non-empty names",
            ));
        }
        Ok(Self { names })
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// Data Source-independent table metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableMetadata {
    /// Unresolved table identifier used for the lookup.
    pub table_reference: TableReference,
    /// Data Source schema mapped into Analytics types.
    pub schema: AnalyticsSchema,
    /// Digest of the Connector's canonical metadata representation.
    pub digest: MetadataDigest,
    /// Data Source state from which this metadata was read.
    pub provenance: MetadataProvenance,
}

impl TableMetadata {
    pub fn new(
        table_reference: TableReference,
        schema: AnalyticsSchema,
        digest: MetadataDigest,
        provenance: MetadataProvenance,
    ) -> Self {
        Self {
            table_reference,
            schema,
            digest,
            provenance,
        }
    }
}

/// Reads catalog state without collapsing absence, partial results, and failures.
pub trait CatalogReader: Send + Sync {
    /// Reads one table without collapsing absence, partial data, or failure.
    fn table_metadata(
        &self,
        table_reference: &TableReference,
    ) -> Result<MetadataState<TableMetadata>, ConnectorError>;
}

/// Maps one Data Source-native type into the Analytics type system.
pub trait TypeMapper<DataSourceType> {
    /// Maps one native type or explains why its semantics cannot be preserved.
    fn map_type(&self, data_source_type: &DataSourceType)
    -> Result<AnalyticsType, UnsupportedType>;
}

/// A Data Source type that cannot be represented without changing its meaning.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("unsupported Data Source type {data_source_type}: {reason}")]
pub struct UnsupportedType {
    /// Data Source-native type description.
    pub data_source_type: String,
    /// Reason its semantics cannot be represented.
    pub reason: String,
}

impl UnsupportedType {
    pub fn new(data_source_type: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            data_source_type: data_source_type.into(),
            reason: reason.into(),
        }
    }
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
}
