//! Text crossing the C ABI.
//!
//! Input text is a NUL-terminated UTF-8 `const char*`, the C convention; no
//! input needs to carry U+0000. Output text carries its length and is also
//! NUL-terminated, so a C caller may use `data` as an ordinary string while
//! text from a Data Source that contains U+0000 still arrives intact through
//! `length`.

use std::ffi::{CStr, c_char};

use connector_sdk::{ConnectorError, SecretString};

use super::invalid_request;

/// UTF-8 text returned by the library.
///
/// `data` is followed by a NUL byte at `data[length]`; `length` excludes it.
/// Absent text has a null `data` and length 0.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorString {
    pub data: *const c_char,
    pub length: usize,
}

impl ScalardbAnalyticsConnectorString {
    pub(super) const ABSENT: Self = Self {
        data: std::ptr::null(),
        length: 0,
    };
}

/// Owned output text with the NUL terminator the C view promises.
pub(super) struct TerminatedText(Box<[u8]>);

impl TerminatedText {
    pub(super) fn new(value: &str) -> Self {
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
        Self(bytes.into_boxed_slice())
    }

    /// Returns the C view; it is valid as long as this value is not dropped.
    /// Moving the value does not move the bytes it points to.
    pub(super) fn view(&self) -> ScalardbAnalyticsConnectorString {
        ScalardbAnalyticsConnectorString {
            data: self.0.as_ptr().cast(),
            length: self.0.len() - 1,
        }
    }
}

/// Reads required input text.
///
/// # Safety
///
/// A non-null `text` points to a NUL-terminated string.
pub(super) unsafe fn read_text(text: *const c_char, name: &str) -> Result<String, ConnectorError> {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { text_bytes(text, name) }?;
    String::from_utf8(bytes.to_vec()).map_err(|_| invalid_request(&format!("{name} is not UTF-8")))
}

/// Reads input text that may be a credential, erasing a rejected copy.
///
/// # Safety
///
/// A non-null `text` points to a NUL-terminated string.
pub(super) unsafe fn read_secret_text(
    text: *const c_char,
    name: &str,
) -> Result<SecretString, ConnectorError> {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { text_bytes(text, name) }?;
    SecretString::from_utf8(bytes.to_vec())
        .ok_or_else(|| invalid_request(&format!("{name} is not UTF-8")))
}

/// Reads optional input text; null and the empty string are absent.
///
/// # Safety
///
/// A non-null `text` points to a NUL-terminated string.
pub(super) unsafe fn read_optional_text(
    text: *const c_char,
    name: &str,
) -> Result<Option<String>, ConnectorError> {
    if text.is_null() {
        return Ok(None);
    }
    // SAFETY: forwarded from the caller.
    let text = unsafe { read_text(text, name) }?;
    Ok((!text.is_empty()).then_some(text))
}

/// # Safety
///
/// A non-null `text` points to a NUL-terminated string.
unsafe fn text_bytes<'a>(text: *const c_char, name: &str) -> Result<&'a [u8], ConnectorError> {
    if text.is_null() {
        return Err(invalid_request(&format!("{name} is null")));
    }
    // SAFETY: forwarded from the caller.
    Ok(unsafe { CStr::from_ptr(text) }.to_bytes())
}
