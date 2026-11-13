//! The C representation of connector errors.

use std::ptr;

use connector_sdk::{ConnectorError, DiagnosticValue, ErrorKind};

use super::{
    ScalardbAnalyticsConnectorBytes, ScalardbAnalyticsConnectorString, text::TerminatedText,
};

/// The kind of a connector error, as defined by the connector contract.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalardbAnalyticsConnectorErrorKind {
    InvalidSettings = 1,
    Connection = 2,
    Authentication = 3,
    Permission = 4,
    InvalidRequest = 5,
    InvalidState = 6,
    StaleRegistration = 7,
    Data = 8,
    Cancellation = 9,
    Timeout = 10,
    Resource = 11,
    DataSource = 12,
    Internal = 13,
}

impl From<ErrorKind> for ScalardbAnalyticsConnectorErrorKind {
    fn from(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::InvalidSettings => Self::InvalidSettings,
            ErrorKind::Connection => Self::Connection,
            ErrorKind::Authentication => Self::Authentication,
            ErrorKind::Permission => Self::Permission,
            ErrorKind::InvalidRequest => Self::InvalidRequest,
            ErrorKind::InvalidState => Self::InvalidState,
            ErrorKind::StaleRegistration => Self::StaleRegistration,
            ErrorKind::Data => Self::Data,
            ErrorKind::Cancellation => Self::Cancellation,
            ErrorKind::Timeout => Self::Timeout,
            ErrorKind::Resource => Self::Resource,
            ErrorKind::DataSource => Self::DataSource,
            ErrorKind::Internal => Self::Internal,
        }
    }
}

/// Which member of a diagnostic value's payload is set.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalardbAnalyticsConnectorDiagnosticType {
    Boolean = 1,
    Uint8 = 2,
    Uint16 = 3,
    Uint32 = 4,
    Uint64 = 5,
    Int8 = 6,
    Int16 = 7,
    Int32 = 8,
    Int64 = 9,
    Float32 = 10,
    Float64 = 11,
    Utf8 = 12,
    Bytes = 13,
}

/// A diagnostic value; read the member named by its type.
#[repr(C)]
#[derive(Clone, Copy)]
pub union ScalardbAnalyticsConnectorDiagnosticPayload {
    pub boolean: bool,
    pub uint8: u8,
    pub uint16: u16,
    pub uint32: u32,
    pub uint64: u64,
    pub int8: i8,
    pub int16: i16,
    pub int32: i32,
    pub int64: i64,
    pub float32: f32,
    pub float64: f64,
    pub utf8: ScalardbAnalyticsConnectorString,
    pub bytes: ScalardbAnalyticsConnectorBytes,
}

/// One diagnostic entry, in the order the connector attached it.
///
/// Keys may repeat; they are details for people and restricted logs, not
/// values to branch on.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorDiagnostic {
    pub key: ScalardbAnalyticsConnectorString,
    pub value_type: ScalardbAnalyticsConnectorDiagnosticType,
    pub value: ScalardbAnalyticsConnectorDiagnosticPayload,
}

/// A connector error.
///
/// `message` is composed by the connector and safe to log. The Data Source
/// code and message are as the Data Source returned them; the message may
/// contain user data, so callers keep it out of ordinary logs. An absent Data
/// Source code or message has a null `data`. Every pointer stays valid as
/// long as the error does.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorError {
    pub kind: ScalardbAnalyticsConnectorErrorKind,
    pub retryable: bool,
    pub message: ScalardbAnalyticsConnectorString,
    pub data_source_code: ScalardbAnalyticsConnectorString,
    pub data_source_message: ScalardbAnalyticsConnectorString,
    pub diagnostics: *const ScalardbAnalyticsConnectorDiagnostic,
    pub diagnostic_count: usize,
}

/// A C error view together with the Rust values its pointers refer to.
///
/// `view` is the first field so that a pointer to it is a pointer to the
/// whole allocation.
#[repr(C)]
pub(crate) struct OwnedError {
    view: ScalardbAnalyticsConnectorError,
    _diagnostics: Box<[ScalardbAnalyticsConnectorDiagnostic]>,
    /// NUL-terminated copies of every text the view points to.
    _texts: Vec<TerminatedText>,
    /// Owns the bytes of `bytes` diagnostic values the view points to.
    _error: ConnectorError,
}

impl OwnedError {
    /// Builds the C view of `error`. The view's pointers refer to heap data
    /// owned by the returned value, which moving the box does not move.
    pub(crate) fn new(error: ConnectorError) -> Box<Self> {
        let mut texts = Vec::new();
        let mut text = |value: &str| {
            let text = TerminatedText::new(value);
            let view = text.view();
            texts.push(text);
            view
        };
        let diagnostics: Box<[ScalardbAnalyticsConnectorDiagnostic]> = error
            .diagnostics()
            .iter()
            .map(|entry| {
                let key = text(entry.key());
                let (value_type, value) = diagnostic_value(entry.value(), &mut text);
                ScalardbAnalyticsConnectorDiagnostic {
                    key,
                    value_type,
                    value,
                }
            })
            .collect();
        let view = ScalardbAnalyticsConnectorError {
            kind: error.kind().into(),
            retryable: error.retryable(),
            message: text(error.safe_message()),
            data_source_code: error
                .data_source_code()
                .map_or(ScalardbAnalyticsConnectorString::ABSENT, &mut text),
            data_source_message: error
                .data_source_message()
                .map_or(ScalardbAnalyticsConnectorString::ABSENT, &mut text),
            diagnostics: if diagnostics.is_empty() {
                ptr::null()
            } else {
                diagnostics.as_ptr()
            },
            diagnostic_count: diagnostics.len(),
        };
        Box::new(Self {
            view,
            _diagnostics: diagnostics,
            _texts: texts,
            _error: error,
        })
    }

    pub(crate) fn view(&self) -> &ScalardbAnalyticsConnectorError {
        &self.view
    }

    /// Hands the error to the caller, who frees it with
    /// `error_free`.
    pub(crate) fn into_raw(error: ConnectorError) -> *mut ScalardbAnalyticsConnectorError {
        Box::into_raw(Self::new(error)).cast()
    }

    /// Frees an error returned by [`OwnedError::into_raw`].
    ///
    /// # Safety
    ///
    /// `error` is null or was returned by [`OwnedError::into_raw`] and not freed.
    pub(crate) unsafe fn free(error: *mut ScalardbAnalyticsConnectorError) {
        if !error.is_null() {
            // SAFETY: the caller guarantees that `error` came from `into_raw`,
            // which leaked a `Box<OwnedError>` whose first field is the view.
            drop(unsafe { Box::from_raw(error.cast::<OwnedError>()) });
        }
    }
}

fn diagnostic_value(
    value: &DiagnosticValue,
    text: &mut impl FnMut(&str) -> ScalardbAnalyticsConnectorString,
) -> (
    ScalardbAnalyticsConnectorDiagnosticType,
    ScalardbAnalyticsConnectorDiagnosticPayload,
) {
    use ScalardbAnalyticsConnectorDiagnosticPayload as Payload;
    use ScalardbAnalyticsConnectorDiagnosticType as Type;
    match value {
        DiagnosticValue::Bool(value) => (Type::Boolean, Payload { boolean: *value }),
        DiagnosticValue::U8(value) => (Type::Uint8, Payload { uint8: *value }),
        DiagnosticValue::U16(value) => (Type::Uint16, Payload { uint16: *value }),
        DiagnosticValue::U32(value) => (Type::Uint32, Payload { uint32: *value }),
        DiagnosticValue::U64(value) => (Type::Uint64, Payload { uint64: *value }),
        DiagnosticValue::I8(value) => (Type::Int8, Payload { int8: *value }),
        DiagnosticValue::I16(value) => (Type::Int16, Payload { int16: *value }),
        DiagnosticValue::I32(value) => (Type::Int32, Payload { int32: *value }),
        DiagnosticValue::I64(value) => (Type::Int64, Payload { int64: *value }),
        DiagnosticValue::F32(value) => (Type::Float32, Payload { float32: *value }),
        DiagnosticValue::F64(value) => (Type::Float64, Payload { float64: *value }),
        DiagnosticValue::Utf8(value) => (Type::Utf8, Payload { utf8: text(value) }),
        DiagnosticValue::Bytes(value) => (
            Type::Bytes,
            Payload {
                bytes: ScalardbAnalyticsConnectorBytes::borrowed(value),
            },
        ),
    }
}
