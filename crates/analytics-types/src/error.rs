//! Errors produced while constructing or converting Analytics types.

use thiserror::Error;

/// A validation or conversion failure in the Analytics type model.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TypeError {
    /// A decimal parameter pair is outside the supported range.
    #[error(
        "invalid DECIMAL({precision}, {scale}); precision must be 1..=76 and a positive scale must not exceed precision"
    )]
    InvalidDecimal { precision: u8, scale: i8 },

    /// Extension metadata appeared without an extension name.
    #[error("Arrow extension metadata is present without an extension name")]
    OrphanExtensionMetadata,

    /// An Arrow field used an extension name not defined by the contract.
    #[error("unknown Analytics Arrow extension {name:?}")]
    UnknownExtensionName { name: String },

    /// An Analytics Arrow extension had missing or malformed parameters.
    #[error("invalid extension metadata for Analytics type {name:?}: {reason}")]
    InvalidExtensionMetadata { name: String, reason: String },

    /// An Analytics Arrow extension used the wrong storage type.
    #[error(
        "Analytics type extension {name:?} requires Arrow storage type {expected}, found {actual}"
    )]
    ExtensionStorageMismatch {
        name: String,
        expected: String,
        actual: String,
    },

    /// The Arrow data type has no Analytics type mapping.
    #[error("unsupported Arrow type {data_type}")]
    UnsupportedArrowType { data_type: String },

    /// A supported Arrow container used a shape that Analytics does not admit.
    #[error("invalid Arrow {container} shape: {reason}")]
    InvalidArrowShape {
        container: &'static str,
        reason: String,
    },
}
