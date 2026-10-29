//! Data Source-independent metadata and connector lifecycle contracts.

use analytics_types::{AnalyticsSchema, AnalyticsType};
use sha2::{Digest, Sha256};

use crate::{ConnectorError, ConnectorExecutor, ConnectorPlanner, TableReference};

/// Completeness of a metadata lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataState<T> {
    Complete(T),
    Incomplete { value: T, missing: Vec<MetadataGap> },
    NotFound,
    Unavailable { reason: String },
}

/// A part of table metadata that the Data Source could not supply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataGap {
    field: String,
    reason: String,
}

impl MetadataGap {
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Stable digest of a connector-defined canonical metadata representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataDigest([u8; 32]);

impl MetadataDigest {
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
    data_source_product: String,
    data_source_version: String,
    catalog_snapshot: Option<String>,
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

    pub fn data_source_product(&self) -> &str {
        &self.data_source_product
    }

    pub fn data_source_version(&self) -> &str {
        &self.data_source_version
    }

    pub fn catalog_snapshot(&self) -> Option<&str> {
        self.catalog_snapshot.as_deref()
    }
}

/// Data Source-independent table metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableMetadata {
    table_reference: TableReference,
    schema: AnalyticsSchema,
    digest: MetadataDigest,
    provenance: MetadataProvenance,
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

    pub fn table_reference(&self) -> &TableReference {
        &self.table_reference
    }

    pub fn schema(&self) -> &AnalyticsSchema {
        &self.schema
    }

    pub fn digest(&self) -> MetadataDigest {
        self.digest
    }

    pub fn provenance(&self) -> &MetadataProvenance {
        &self.provenance
    }
}

/// Reads catalog state without collapsing absence, partial results, and failures.
pub trait CatalogReader: Send + Sync {
    fn table_metadata(
        &self,
        table_reference: &TableReference,
    ) -> Result<MetadataState<TableMetadata>, ConnectorError>;
}

/// Maps one Data Source-native type into the Analytics type system.
pub trait TypeMapper<DataSourceType> {
    fn map_type(&self, data_source_type: &DataSourceType)
    -> Result<AnalyticsType, UnsupportedType>;
}

/// A Data Source type that cannot be represented without changing its meaning.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("unsupported Data Source type {data_source_type}: {reason}")]
pub struct UnsupportedType {
    data_source_type: String,
    reason: String,
}

impl UnsupportedType {
    pub fn new(data_source_type: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            data_source_type: data_source_type.into(),
            reason: reason.into(),
        }
    }

    pub fn data_source_type(&self) -> &str {
        &self.data_source_type
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Process-level connector factory.
pub trait ConnectorLibrary: Send + Sync {
    type DataSourceConfig;
    type DataSource: DataSource;

    fn open_data_source(
        &self,
        config: Self::DataSourceConfig,
    ) -> Result<Self::DataSource, ConnectorError>;
}

/// Long-lived Data Source handle that can create isolated sessions.
pub trait DataSource: Send + Sync {
    type SessionConfig;
    type Session: DataSourceSession;

    fn open_session(&self, config: Self::SessionConfig) -> Result<Self::Session, ConnectorError>;
}

/// Session-scoped catalog, planning, and execution capabilities.
pub trait DataSourceSession: Send + Sync {
    type Catalog: CatalogReader;
    type Planner: ConnectorPlanner;
    type Executor: ConnectorExecutor;

    fn catalog(&self) -> &Self::Catalog;
    fn planner(&self) -> &Self::Planner;
    fn executor(&self) -> &Self::Executor;
}
