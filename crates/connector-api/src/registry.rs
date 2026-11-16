//! The connectors built into this library and the static information about them.

use std::sync::OnceLock;

use connector_sdk::{DataSourceAccess, OptionValueType, runtime::BuiltInConnector};
use serde_json::{Value, json};

/// The version of the static information document's layout.
const STATIC_INFORMATION_VERSION: u32 = 1;

/// Returns the connectors selected by this build's features.
///
/// A Connector crate registers here behind its feature; nothing else in this
/// crate knows about a particular Data Source.
pub(crate) fn built_in_connectors() -> &'static [BuiltInConnector] {
    static CONNECTORS: OnceLock<Vec<BuiltInConnector>> = OnceLock::new();
    CONNECTORS.get_or_init(|| {
        vec![
            #[cfg(feature = "test-connector")]
            BuiltInConnector::of::<connector_sdk::test_connector::TestConnector>(),
            #[cfg(feature = "postgresql")]
            BuiltInConnector::of::<connector_postgresql::PostgresConnector>(),
            #[cfg(feature = "mysql")]
            BuiltInConnector::of::<connector_mysql::MysqlConnector>(),
            #[cfg(feature = "salesforce")]
            BuiltInConnector::of::<connector_salesforce::SalesforceConnector>(),
            #[cfg(feature = "sqlserver-odbc")]
            BuiltInConnector::of::<connector_sqlserver_odbc::SqlServerConnector>(),
        ]
    })
}

/// Returns the static information JSON document.
///
/// It depends only on the build, so it is available before initialization.
pub(crate) fn static_information() -> &'static str {
    static DOCUMENT: OnceLock<String> = OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let connectors: Vec<Value> = built_in_connectors().iter().map(describe).collect();
        json!({
            "static_information_version": STATIC_INFORMATION_VERSION,
            "library_version": env!("CARGO_PKG_VERSION"),
            "connectors": connectors,
        })
        .to_string()
    })
}

/// Describes one built-in connector for the static information.
fn describe(connector: &BuiltInConnector) -> Value {
    let descriptor = &connector.descriptor;
    let options: Vec<Value> = descriptor
        .options
        .iter()
        .map(|option| {
            json!({
                "key": option.key,
                "secret": option.secret,
                "required": option.required,
                "value_type": match option.value_type {
                    OptionValueType::String => "string",
                    OptionValueType::Integer => "integer",
                    OptionValueType::Boolean => "boolean",
                },
                "default": option.default,
                "description": option.description,
            })
        })
        .collect();
    let access = match &descriptor.access {
        DataSourceAccess::Adbc {
            driver,
            supported_versions,
        } => json!({
            "form": "adbc",
            "driver": driver,
            "supported_driver_versions": supported_versions,
        }),
        DataSourceAccess::Direct => json!({ "form": "direct" }),
    };
    json!({
        "name": descriptor.name,
        "version": descriptor.version,
        "connectors": descriptor.connectors,
        "options": options,
        "data_source_access": access,
        "source_metadata_schema": connector.source_metadata_schema,
    })
}
