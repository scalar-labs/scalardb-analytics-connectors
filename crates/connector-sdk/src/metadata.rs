//! Source-independent metadata and connector lifecycle contracts.

use analytics_types::{AnalyticsSchema, AnalyticsType};
use sha2::{Digest, Sha256};

use crate::{ConnectorError, ConnectorExecutor, ConnectorPlanner, SourceBinding};

/// Completeness of a metadata lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataState<T> {
    Complete(T),
    Incomplete { value: T, missing: Vec<MetadataGap> },
    NotFound,
    Unavailable { reason: String },
}

/// A part of table metadata that the source could not supply.
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
    source_product: String,
    source_version: String,
    catalog_snapshot: Option<String>,
}

impl MetadataProvenance {
    pub fn new(
        source_product: impl Into<String>,
        source_version: impl Into<String>,
        catalog_snapshot: Option<String>,
    ) -> Self {
        Self {
            source_product: source_product.into(),
            source_version: source_version.into(),
            catalog_snapshot,
        }
    }

    pub fn source_product(&self) -> &str {
        &self.source_product
    }

    pub fn source_version(&self) -> &str {
        &self.source_version
    }

    pub fn catalog_snapshot(&self) -> Option<&str> {
        self.catalog_snapshot.as_deref()
    }
}

/// Source-independent table metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableMetadata {
    binding: SourceBinding,
    schema: AnalyticsSchema,
    digest: MetadataDigest,
    provenance: MetadataProvenance,
}

impl TableMetadata {
    pub fn new(
        binding: SourceBinding,
        schema: AnalyticsSchema,
        digest: MetadataDigest,
        provenance: MetadataProvenance,
    ) -> Self {
        Self {
            binding,
            schema,
            digest,
            provenance,
        }
    }

    pub fn binding(&self) -> &SourceBinding {
        &self.binding
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
        binding: &SourceBinding,
    ) -> Result<MetadataState<TableMetadata>, ConnectorError>;
}

/// Maps one source-native type into the Analytics type system.
pub trait TypeMapper<SourceType> {
    fn map_type(&self, source_type: &SourceType) -> Result<AnalyticsType, UnsupportedType>;
}

/// A source type that cannot be represented without changing its meaning.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("unsupported source type {source_type}: {reason}")]
pub struct UnsupportedType {
    source_type: String,
    reason: String,
}

impl UnsupportedType {
    pub fn new(source_type: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            source_type: source_type.into(),
            reason: reason.into(),
        }
    }

    pub fn source_type(&self) -> &str {
        &self.source_type
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Process-level connector factory.
pub trait ConnectorLibrary: Send + Sync {
    type SourceConfig;
    type Source: ConnectorSource;

    fn open_source(&self, config: Self::SourceConfig) -> Result<Self::Source, ConnectorError>;
}

/// Long-lived source handle that can create isolated sessions.
pub trait ConnectorSource: Send + Sync {
    type SessionConfig;
    type Session: ConnectorSession;

    fn open_session(&self, config: Self::SessionConfig) -> Result<Self::Session, ConnectorError>;
}

/// Session-scoped catalog, planning, and execution capabilities.
pub trait ConnectorSession: Send + Sync {
    type Catalog: CatalogReader;
    type Planner: ConnectorPlanner;
    type Executor: ConnectorExecutor;

    fn catalog(&self) -> &Self::Catalog;
    fn planner(&self) -> &Self::Planner;
    fn executor(&self) -> &Self::Executor;
}
