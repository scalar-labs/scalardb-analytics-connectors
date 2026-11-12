//! What a Connector declares about itself and receives when its units open.
//!
//! A connector states what it accepts in its [`ConnectorDescriptor`]; the API
//! validates caller options against it before the connector sees them.

mod descriptor;
mod options;

pub use descriptor::{ConnectorDescriptor, DataSourceAccess, OptionDescriptor, OptionValueType};
pub use options::{ConnectionOptions, OptionValue, SecretString};

use std::path::PathBuf;

/// Process-wide settings received when the connector API library is initialized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibrarySettings {
    /// Directory from which ADBC drivers are loaded; absent when none is configured.
    pub adbc_driver_directory: Option<PathBuf>,
}

/// The name and version of one component that produced table metadata.
///
/// Retrieval info names the connector from its [`ConnectorDescriptor`], the
/// driver from [`crate::Access::component`], and the library
/// version from the connector API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: String,
    pub version: String,
}
