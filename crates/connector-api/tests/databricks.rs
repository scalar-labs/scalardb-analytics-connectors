//! The Databricks Connector through the C ABI, when the `databricks` feature
//! builds it into the library.
//!
//! No workspace or driver is needed: the test checks the static information
//! and that opening a Data Source validates its options before the driver is
//! loaded.

#![cfg(feature = "databricks")]

mod support;

use connector_api::abi::*;
use serde_json::Value;
use support::*;

const TOKEN: &str = "dapi-connector-test-secret";

#[test]
fn the_databricks_connector_is_described_and_validates_its_options() {
    let information: Value =
        serde_json::from_str(&read_string((api().static_information)()).unwrap()).unwrap();
    let databricks = information["connectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connector| connector["name"] == "databricks")
        .expect("the static information lists the Databricks Connector");
    assert_eq!(databricks["connectors"][0], "databricks");
    assert_eq!(databricks["data_source_access"]["form"], "adbc");
    assert_eq!(
        databricks["data_source_access"]["driver"],
        "adbc_driver_databricks"
    );
    let option = |key: &str| {
        databricks["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|option| option["key"] == key)
            .unwrap()
            .clone()
    };
    assert_eq!(option("access_token")["secret"], true);
    assert_eq!(option("client_secret")["secret"], true);
    assert_eq!(option("host")["required"], true);
    assert!(databricks["source_metadata_schema"].is_object());

    initialize();
    let base = [
        ("host", "adb-1.2.azuredatabricks.net"),
        ("http_path", "/sql/1.0/warehouses/abc"),
    ];
    let error = DataSource::open_connector(
        "databricks",
        &[
            base[0],
            base[1],
            ("access_token", TOKEN),
            ("client_id", "principal"),
        ],
    )
    .err()
    .expect("two authentications are refused");
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidSettings
    );
    assert!(
        error.message.contains("`access_token`"),
        "{}",
        error.message
    );
    assert!(!error.message.contains(TOKEN));

    // The library has no ADBC driver directory, so valid options reach the
    // driver's loading, which fails.
    let error =
        DataSource::open_connector("databricks", &[base[0], base[1], ("access_token", TOKEN)])
            .err()
            .expect("no driver is installed");
    assert_eq!(
        error.kind,
        ScalardbAnalyticsConnectorErrorKind::InvalidSettings
    );
    assert!(error.message.contains("driver"), "{}", error.message);
    assert!(!error.message.contains(TOKEN));
}
