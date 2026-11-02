use std::os::raw::c_char;

use adbc_core::error::{Error, Status};
use connector_adbc::{AdbcDetail, AdbcDetails, AdbcDiagnostic, AdbcStatus, classify_error};
use connector_sdk::{ConnectorError, DiagnosticValue, ErrorKind};

fn diagnostic(status: AdbcStatus, sqlstate: [u8; 5]) -> AdbcDiagnostic {
    AdbcDiagnostic::new(status, "driver message", sqlstate, 0, AdbcDetails::Missing)
}

fn values<'a>(error: &'a ConnectorError, key: &str) -> Vec<&'a DiagnosticValue> {
    error
        .diagnostics()
        .iter()
        .filter(|entry| entry.key() == key)
        .map(|entry| entry.value())
        .collect()
}

fn value<'a>(error: &'a ConnectorError, key: &str) -> &'a DiagnosticValue {
    let matches = values(error, key);
    assert_eq!(matches.len(), 1, "expected one diagnostic entry for {key}");
    matches[0]
}

#[test]
fn classifies_standard_sqlstate_classes() {
    let cases = [
        (*b"08001", ErrorKind::Connection, true),
        (*b"28000", ErrorKind::Authentication, false),
        (*b"22003", ErrorKind::DataSource, false),
        (*b"40001", ErrorKind::DataSource, true),
        (*b"42000", ErrorKind::Internal, false),
        (*b"HY000", ErrorKind::DataSource, false),
    ];

    for (sqlstate, expected_kind, expected_retryable) in cases {
        let error = classify_error(diagnostic(AdbcStatus::Known(Status::InvalidData), sqlstate));

        assert_eq!(error.kind(), expected_kind, "SQLSTATE {sqlstate:?}");
        assert_eq!(
            error.retryable(),
            expected_retryable,
            "SQLSTATE {sqlstate:?}"
        );
        assert_eq!(
            error.data_source_code(),
            Some(std::str::from_utf8(&sqlstate).unwrap())
        );
    }
}

#[test]
fn preserves_user_meaningful_fields_in_cross_language_diagnostics() {
    let error = classify_error(AdbcDiagnostic::new(
        AdbcStatus::Known(Status::InvalidData),
        "value 42 is out of range",
        *b"22003",
        1234,
        AdbcDetails::Present(vec![
            AdbcDetail::new("position", [4, 2]),
            AdbcDetail::new("position", [8, 4]),
        ]),
    ));

    assert_eq!(error.data_source_code(), Some("22003"));
    assert_eq!(
        error.data_source_message(),
        Some("value 42 is out of range")
    );
    assert_eq!(value(&error, "adbc.status"), &DiagnosticValue::U8(7));
    assert_eq!(
        value(&error, "adbc.sqlstate"),
        &DiagnosticValue::Bytes(b"22003".to_vec())
    );
    assert_eq!(
        value(&error, "adbc.vendor_code"),
        &DiagnosticValue::I32(1234)
    );
    assert_eq!(
        value(&error, "adbc.details.state"),
        &DiagnosticValue::Utf8("present".to_owned())
    );
    assert_eq!(
        values(&error, "adbc.detail/position"),
        vec![
            &DiagnosticValue::Bytes(vec![4, 2]),
            &DiagnosticValue::Bytes(vec![8, 4]),
        ]
    );
    assert_eq!(
        error
            .diagnostics()
            .iter()
            .map(|entry| entry.key())
            .collect::<Vec<_>>(),
        [
            "adbc.status",
            "adbc.sqlstate",
            "adbc.vendor_code",
            "adbc.details.state",
            "adbc.detail/position",
            "adbc.detail/position",
        ]
    );
    assert!(!format!("{error:?}").contains("value 42"));
    assert!(!error.to_string().contains("value 42"));
}

#[test]
fn missing_sqlstate_uses_status_without_erasing_raw_value() {
    let error = classify_error(AdbcDiagnostic::new(
        AdbcStatus::Known(Status::Timeout),
        "request exceeded deadline",
        [0; 5],
        17,
        AdbcDetails::Missing,
    ));

    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(error.retryable());
    assert_eq!(error.data_source_code(), None);
    assert_eq!(
        value(&error, "adbc.sqlstate"),
        &DiagnosticValue::Bytes(vec![0; 5])
    );
    assert_eq!(value(&error, "adbc.vendor_code"), &DiagnosticValue::I32(17));
    assert_eq!(
        value(&error, "adbc.details.state"),
        &DiagnosticValue::Utf8("missing".to_owned())
    );
}

#[test]
fn malformed_diagnostics_become_internal_without_dropping_captured_fields() {
    let cases = [
        AdbcDiagnostic::new(
            AdbcStatus::Unknown(99),
            "future status",
            *b"22003",
            41,
            AdbcDetails::Missing,
        ),
        AdbcDiagnostic::new(
            AdbcStatus::Known(Status::Ok),
            "unexpected success",
            *b"22003",
            42,
            AdbcDetails::Missing,
        ),
        AdbcDiagnostic::new(
            AdbcStatus::Known(Status::InvalidData),
            "bad sqlstate",
            *b"22!03",
            43,
            AdbcDetails::Missing,
        ),
        AdbcDiagnostic::new(
            AdbcStatus::Known(Status::InvalidData),
            "bad details",
            *b"22003",
            44,
            AdbcDetails::Malformed {
                captured: vec![AdbcDetail::new("row", [1]), AdbcDetail::new("row", [2])],
                reason: "detail key was not UTF-8".to_owned(),
            },
        ),
    ];

    for (index, diagnostic) in cases.into_iter().enumerate() {
        let expected_status = diagnostic.status();
        let expected_sqlstate = diagnostic.sqlstate_bytes().to_vec();
        let expected_vendor = diagnostic.vendor_code();
        let expected_message = diagnostic.message().to_owned();
        let error = classify_error(diagnostic);

        assert_eq!(error.kind(), ErrorKind::Internal, "case {index}");
        assert!(!error.retryable(), "case {index}");
        assert_eq!(
            error.data_source_message(),
            Some(expected_message.as_str()),
            "case {index}"
        );
        assert_eq!(
            value(&error, "adbc.status"),
            &DiagnosticValue::U8(match expected_status {
                AdbcStatus::Known(status) => status.into(),
                AdbcStatus::Unknown(code) => code,
            }),
            "case {index}"
        );
        assert_eq!(
            value(&error, "adbc.sqlstate"),
            &DiagnosticValue::Bytes(expected_sqlstate),
            "case {index}"
        );
        assert_eq!(
            value(&error, "adbc.vendor_code"),
            &DiagnosticValue::I32(expected_vendor),
            "case {index}"
        );
    }

    let malformed_details = classify_error(AdbcDiagnostic::new(
        AdbcStatus::Known(Status::InvalidData),
        "bad details",
        *b"22003",
        44,
        AdbcDetails::Malformed {
            captured: vec![AdbcDetail::new("row", [1]), AdbcDetail::new("row", [2])],
            reason: "detail key was not UTF-8".to_owned(),
        },
    ));
    assert_eq!(
        value(&malformed_details, "adbc.details.state"),
        &DiagnosticValue::Utf8("malformed".to_owned())
    );
    assert_eq!(
        value(&malformed_details, "adbc.details.error"),
        &DiagnosticValue::Utf8("detail key was not UTF-8".to_owned())
    );
    assert_eq!(
        values(&malformed_details, "adbc.detail/row"),
        vec![
            &DiagnosticValue::Bytes(vec![1]),
            &DiagnosticValue::Bytes(vec![2]),
        ]
    );
}

#[test]
fn converts_adbc_error_without_losing_duplicate_details() {
    let source = Error {
        message: "duplicate detail keys".to_owned(),
        status: Status::InvalidData,
        vendor_code: 77,
        sqlstate: b"22000".map(|byte| byte as c_char),
        details: Some(vec![
            ("row".to_owned(), vec![1]),
            ("row".to_owned(), vec![2]),
        ]),
    };

    let error = classify_error(source.into());

    assert_eq!(error.data_source_message(), Some("duplicate detail keys"));
    assert_eq!(
        values(&error, "adbc.detail/row"),
        vec![
            &DiagnosticValue::Bytes(vec![1]),
            &DiagnosticValue::Bytes(vec![2]),
        ]
    );
}

#[test]
fn captured_diagnostic_debug_output_is_redacted() {
    let diagnostic = AdbcDiagnostic::new(
        AdbcStatus::Known(Status::InvalidData),
        "secret source message",
        *b"22003",
        1234,
        AdbcDetails::Present(vec![AdbcDetail::new("secret-key", b"secret-value")]),
    );

    let debug = format!("{diagnostic:?}");
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("22003"));
    assert!(!debug.contains("1234"));
}
