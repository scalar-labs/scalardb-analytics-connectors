//! What a connector declares about itself in static information.

/// A built-in connector's self-description.
///
/// The connector API publishes it as static information and validates the
/// options of every Data Source opened through this connector against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorDescriptor {
    /// The connector's name.
    pub name: String,
    /// The connector's version; a changed type mapping or fixed setting raises it.
    pub version: String,
    /// The Connector names a caller may pass when opening a Data Source.
    pub connectors: Vec<String>,
    /// Every connection option the connector accepts.
    pub options: Vec<OptionDescriptor>,
    /// How the connector reaches its Data Source.
    pub access: DataSourceAccess,
}

/// One accepted connection option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionDescriptor {
    pub key: String,
    /// Whether the value is a credential that callers hide from display and logs.
    pub secret: bool,
    pub required: bool,
    pub value_type: OptionValueType,
    /// The value the connector uses when the option is omitted, for display.
    pub default: Option<String>,
    pub description: String,
}

/// The textual form an option value must take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionValueType {
    /// Any UTF-8 text.
    String,
    /// A decimal signed 64-bit integer.
    Integer,
    /// `true` or `false`.
    Boolean,
}

/// The form of Data Source access a connector uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataSourceAccess {
    /// Access through a separately distributed ADBC driver.
    Adbc {
        /// The ADBC driver the connector loads.
        driver: String,
        /// The driver versions the connector supports, as displayed to users.
        supported_versions: String,
    },
    /// Access through reader crates or a client built into the library.
    Direct,
}
