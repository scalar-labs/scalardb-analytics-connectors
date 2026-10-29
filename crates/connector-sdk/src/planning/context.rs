//! Data Source and session semantics that affect pushdown validity.

/// Null ordering in the Query Engine session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NullOrdering {
    NullsFirst,
    NullsLast,
    DataSourceDefault,
}

/// Concrete Data Source and session semantics for one planning attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanningContext {
    data_source_product: String,
    data_source_version: String,
    execution_domain: String,
    collation: Option<String>,
    time_zone: Option<String>,
    decimal_semantics: Option<String>,
    null_ordering: NullOrdering,
    snapshot_fingerprint: Option<String>,
    security_fingerprint: String,
}

impl PlanningContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_source_product: impl Into<String>,
        data_source_version: impl Into<String>,
        execution_domain: impl Into<String>,
        collation: Option<String>,
        time_zone: Option<String>,
        decimal_semantics: Option<String>,
        null_ordering: NullOrdering,
        snapshot_fingerprint: Option<String>,
        security_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            data_source_product: data_source_product.into(),
            data_source_version: data_source_version.into(),
            execution_domain: execution_domain.into(),
            collation,
            time_zone,
            decimal_semantics,
            null_ordering,
            snapshot_fingerprint,
            security_fingerprint: security_fingerprint.into(),
        }
    }

    pub fn data_source_product(&self) -> &str {
        &self.data_source_product
    }
    pub fn data_source_version(&self) -> &str {
        &self.data_source_version
    }
    pub fn execution_domain(&self) -> &str {
        &self.execution_domain
    }
    pub fn collation(&self) -> Option<&str> {
        self.collation.as_deref()
    }
    pub fn time_zone(&self) -> Option<&str> {
        self.time_zone.as_deref()
    }
    pub fn decimal_semantics(&self) -> Option<&str> {
        self.decimal_semantics.as_deref()
    }
    pub fn null_ordering(&self) -> NullOrdering {
        self.null_ordering
    }
    pub fn snapshot_fingerprint(&self) -> Option<&str> {
        self.snapshot_fingerprint.as_deref()
    }
    pub fn security_fingerprint(&self) -> &str {
        &self.security_fingerprint
    }

    pub(crate) fn fingerprint_fields(&self) -> [&str; 9] {
        [
            &self.data_source_product,
            &self.data_source_version,
            &self.execution_domain,
            self.collation.as_deref().unwrap_or_default(),
            self.time_zone.as_deref().unwrap_or_default(),
            self.decimal_semantics.as_deref().unwrap_or_default(),
            match self.null_ordering {
                NullOrdering::NullsFirst => "nulls-first",
                NullOrdering::NullsLast => "nulls-last",
                NullOrdering::DataSourceDefault => "data-source-default",
            },
            self.snapshot_fingerprint.as_deref().unwrap_or_default(),
            &self.security_fingerprint,
        ]
    }
}

/// An unresolved non-empty table name path.
///
/// Metadata requests and Substrait `NamedTable` relations use this carrier.
/// It does not prove membership in an open Data Source or registered schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableReference {
    names: Vec<String>,
}

impl TableReference {
    pub(crate) fn new(names: Vec<String>) -> Self {
        Self { names }
    }
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// A digest binding a plan to the contract and planning context it was checked under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidationFingerprint([u8; 32]);

impl ValidationFingerprint {
    pub(crate) fn new(value: [u8; 32]) -> Self {
        Self(value)
    }
    pub fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
