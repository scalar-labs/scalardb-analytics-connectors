use connector_sdk::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorKind};

#[test]
fn preserves_an_adbc_like_error_without_exposing_source_details_in_formatting() {
    let error = ConnectorError::new(
        ErrorKind::Permission,
        false,
        "the source denied access to the requested target",
    )
    .with_source_error(
        Some("42501".to_owned()),
        Some("permission denied for relation customer_secrets".to_owned()),
    )
    .with_diagnostics([
        DiagnosticEntry::new("adbc.status", DiagnosticValue::U8(4)),
        DiagnosticEntry::new("adbc.sqlstate", DiagnosticValue::Utf8("42501".to_owned())),
        DiagnosticEntry::new("adbc.vendor_code", DiagnosticValue::I32(0)),
        DiagnosticEntry::new(
            "adbc.detail",
            DiagnosticValue::Bytes(vec![0x00, 0xff, 0x10]),
        ),
        DiagnosticEntry::new("adbc.detail", DiagnosticValue::Bytes(vec![0x20, 0x30])),
    ]);

    assert_eq!(error.kind(), ErrorKind::Permission);
    assert!(!error.retryable());
    assert_eq!(error.source_code(), Some("42501"));
    assert_eq!(
        error.source_message(),
        Some("permission denied for relation customer_secrets")
    );
    assert_eq!(
        error
            .diagnostics()
            .iter()
            .map(DiagnosticEntry::key)
            .collect::<Vec<_>>(),
        [
            "adbc.status",
            "adbc.sqlstate",
            "adbc.vendor_code",
            "adbc.detail",
            "adbc.detail",
        ]
    );
    assert_eq!(
        error.diagnostics()[3].value(),
        &DiagnosticValue::Bytes(vec![0x00, 0xff, 0x10])
    );
    assert_eq!(
        error.diagnostics()[4].value(),
        &DiagnosticValue::Bytes(vec![0x20, 0x30])
    );

    for formatted in [error.to_string(), format!("{error:?}")] {
        assert!(!formatted.contains("customer_secrets"));
        assert!(!formatted.contains("adbc.detail"));
        assert!(!formatted.contains("255"));
    }
}

#[test]
fn preserves_a_direct_s3_like_service_error() {
    let error = ConnectorError::new(
        ErrorKind::Permission,
        false,
        "the object store denied access to the requested object",
    )
    .with_source_error(
        Some("AccessDenied".to_owned()),
        Some("Access Denied for s3://private/customer.csv".to_owned()),
    )
    .with_diagnostics([
        DiagnosticEntry::new("http.status", DiagnosticValue::U16(403)),
        DiagnosticEntry::new(
            "aws.request_id",
            DiagnosticValue::Utf8("request-123".to_owned()),
        ),
        DiagnosticEntry::new(
            "aws.extended_request_id",
            DiagnosticValue::Utf8("extended-456".to_owned()),
        ),
    ]);

    assert_eq!(error.source_code(), Some("AccessDenied"));
    assert_eq!(
        error.source_message(),
        Some("Access Denied for s3://private/customer.csv")
    );
    assert_eq!(
        error.diagnostics(),
        [
            DiagnosticEntry::new("http.status", DiagnosticValue::U16(403)),
            DiagnosticEntry::new(
                "aws.request_id",
                DiagnosticValue::Utf8("request-123".to_owned()),
            ),
            DiagnosticEntry::new(
                "aws.extended_request_id",
                DiagnosticValue::Utf8("extended-456".to_owned()),
            ),
        ]
    );
    assert!(!error.to_string().contains("private/customer.csv"));
    assert!(!format!("{error:?}").contains("request-123"));
}

#[test]
fn represents_each_diagnostic_scalar_type_without_coercion() {
    let values = vec![
        DiagnosticValue::Bool(true),
        DiagnosticValue::U8(u8::MAX),
        DiagnosticValue::U16(u16::MAX),
        DiagnosticValue::U32(u32::MAX),
        DiagnosticValue::U64(u64::MAX),
        DiagnosticValue::I8(i8::MIN),
        DiagnosticValue::I16(i16::MIN),
        DiagnosticValue::I32(i32::MIN),
        DiagnosticValue::I64(i64::MIN),
        DiagnosticValue::F32(1.25),
        DiagnosticValue::F64(2.5),
        DiagnosticValue::Utf8("value".to_owned()),
        DiagnosticValue::Bytes(vec![0x00, 0xff]),
    ];
    let diagnostics = values
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, value)| DiagnosticEntry::new(index.to_string(), value));
    let error = ConnectorError::new(ErrorKind::Source, true, "source request failed")
        .with_diagnostics(diagnostics);

    assert_eq!(
        error
            .diagnostics()
            .iter()
            .map(DiagnosticEntry::value)
            .collect::<Vec<_>>(),
        values.iter().collect::<Vec<_>>()
    );
}
