//! Opening a Data Source: the options, the open-time check, and its timeout.

use std::time::{Duration, Instant};

use connector_sdk::{ErrorKind, SecretString};

use super::{MockDynamoDb, SECRET_ACCESS_KEY, assert_no_secret, error, open, options};

fn without(options: Vec<(String, SecretString)>, key: &str) -> Vec<(String, SecretString)> {
    options
        .into_iter()
        .filter(|(name, _)| name != key)
        .collect()
}

#[test]
fn opening_checks_the_credentials_with_one_list_tables_request() {
    let mock = MockDynamoDb::start();
    open(options(&mock.url()), None).unwrap();
    let bodies = mock.bodies("ListTables");
    assert_eq!(bodies, vec![serde_json::json!({ "Limit": 1 })]);
}

#[test]
fn rejected_credentials_fail_the_open_as_authentication() {
    let mock = MockDynamoDb::start();
    mock.on(
        "ListTables",
        Some(serde_json::json!({ "Limit": 1 })),
        error(
            400,
            "UnrecognizedClientException",
            "The security token included in the request is invalid.",
        ),
    );
    let error = open(options(&mock.url()), None).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::Authentication);
    assert!(!error.retryable());
    assert_eq!(
        error.data_source_code(),
        Some("UnrecognizedClientException")
    );
    assert_no_secret(&error);
}

#[test]
fn credentials_that_may_not_list_tables_still_open() {
    let mock = MockDynamoDb::start();
    mock.on(
        "ListTables",
        Some(serde_json::json!({ "Limit": 1 })),
        error(
            400,
            "AccessDeniedException",
            "not authorized: dynamodb:ListTables",
        ),
    );
    open(options(&mock.url()), None).unwrap();
}

#[test]
fn the_connection_timeout_bounds_the_open_time_check() {
    let mock = MockDynamoDb::start();
    mock.on(
        "ListTables",
        Some(serde_json::json!({ "Limit": 1 })),
        super::ok(serde_json::json!({ "TableNames": [] })).set_delay(Duration::from_secs(10)),
    );
    let started = Instant::now();
    let error = open(options(&mock.url()), Some(Duration::from_millis(300)))
        .err()
        .unwrap();
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(error.retryable());
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn an_unreachable_endpoint_fails_the_open_as_connection() {
    // Port 9 (discard) is closed on test hosts, so connecting is refused.
    let error = open(options("http://127.0.0.1:9"), None).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::Connection);
    assert!(error.retryable());
}

#[test]
fn incomplete_static_credentials_are_invalid_settings() {
    let url = "http://127.0.0.1:9";
    let token = (
        "session_token".to_owned(),
        SecretString::new("token".to_owned()),
    );
    let cases = [
        without(options(url), "secret_access_key"),
        without(options(url), "access_key_id"),
        without(without(options(url), "access_key_id"), "secret_access_key")
            .into_iter()
            .chain([token])
            .collect(),
        without(options(url), "region"),
    ];
    for options in cases {
        let error = open(options, None).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidSettings, "{error}");
        assert_no_secret(&error);
    }
}

#[test]
fn an_endpoint_must_be_an_http_url() {
    let mut options = without(options("unused"), "endpoint");
    options.push((
        "endpoint".to_owned(),
        SecretString::new("dynamodb.us-east-1.amazonaws.com".to_owned()),
    ));
    let error = open(options, None).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::InvalidSettings);
    assert!(!error.safe_message().contains(SECRET_ACCESS_KEY));
}

#[test]
fn a_malformed_endpoint_or_region_is_invalid_settings() {
    let with = |key: &str, value: &str| {
        let mut options = without(options("http://127.0.0.1:9"), key);
        options.push((key.to_owned(), SecretString::new(value.to_owned())));
        options
    };
    let cases = [
        with("endpoint", "http://exa mple.com"),
        with("endpoint", "http://"),
        with("endpoint", "http://[::1"),
        with("region", "us east 1"),
        with("region", ""),
    ];
    for options in cases {
        let error = open(options, None).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidSettings, "{error}");
    }
}
