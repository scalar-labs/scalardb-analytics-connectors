//! The connector API's C functions and types.
//!
//! This module is the only place that handles raw pointers. Each function
//! checks its pointer arguments, converts them to safe values, calls the
//! runtime, and contains any panic so that none crosses the C ABI.
//!
//! Conventions shared by every function:
//!
//! - A function that can fail returns null on success, or an error the caller
//!   frees with `scalardb_analytics_connector_error_free`.
//! - Input text is a NUL-terminated UTF-8 `const char*`.
//! - Output text is UTF-8 with a length and a NUL terminator after it; absent
//!   output text has a null `data`.
//! - Binary data, such as Substrait plans, carries a length and no terminator.
//! - The library copies every input during the call.
//! - A timeout of 0 milliseconds means no limit.

mod error;
mod stream;
mod text;

pub use error::{
    ScalardbAnalyticsConnectorDiagnostic, ScalardbAnalyticsConnectorDiagnosticPayload,
    ScalardbAnalyticsConnectorDiagnosticType, ScalardbAnalyticsConnectorError,
    ScalardbAnalyticsConnectorErrorKind,
};
pub use text::ScalardbAnalyticsConnectorString;

use std::{
    ffi::c_char,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    ptr, slice,
    sync::{Arc, OnceLock},
    time::Duration,
};

use arrow_array::ffi_stream::FFI_ArrowArrayStream;
use connector_sdk::{
    ConnectorError, ErrorKind, LibrarySettings,
    runtime::{
        DataSourceState, PlanningOutcome, ReadOperation, SessionState, SharedRemoteExecution,
        panic_error,
    },
};

use crate::{library, registry};
use error::OwnedError;
use text::{TerminatedText, read_optional_text, read_secret_text, read_text};

/// Borrowed bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ScalardbAnalyticsConnectorBytes {
    pub data: *const u8,
    pub length: usize,
}

impl ScalardbAnalyticsConnectorBytes {
    const ABSENT: Self = Self {
        data: ptr::null(),
        length: 0,
    };

    fn borrowed(value: &[u8]) -> Self {
        Self {
            data: value.as_ptr(),
            length: value.len(),
        }
    }
}

/// Bytes owned by the caller, freed with `scalardb_analytics_connector_owned_bytes_free`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorOwnedBytes {
    pub data: *mut u8,
    pub length: usize,
}

/// Settings received once per process at initialization.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorLibrarySettings {
    /// Directory from which ADBC drivers are loaded; null or empty when none is
    /// configured.
    pub adbc_driver_directory: *const c_char,
}

/// One connection option passed when opening a Data Source.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorOption {
    pub key: *const c_char,
    pub value: *const c_char,
}

/// An opened Data Source. It may be used from several threads at once.
pub struct ScalardbAnalyticsConnectorDataSource {
    state: Arc<DataSourceState>,
}

/// A session: one connection, running at most one operation at a time.
pub struct ScalardbAnalyticsConnectorSession {
    state: Arc<SessionState>,
}

/// The result of planning one pushdown request.
pub struct ScalardbAnalyticsConnectorPushdownResult {
    outcome: PlanningOutcome,
    /// The unsupported reason as output text, when the request was not planned.
    unsupported_reason: Option<TerminatedText>,
}

/// The part of a planned pushdown that a read executes.
pub struct ScalardbAnalyticsConnectorRemoteExecution {
    remote: SharedRemoteExecution,
}

/// A read operation. It may be cancelled from any thread.
pub struct ScalardbAnalyticsConnectorOperation {
    operation: Arc<ReadOperation>,
    /// The C view of the operation's error, built on first retrieval.
    error: OnceLock<Box<OwnedError>>,
}

/// Returns the static information JSON document.
///
/// It is available before initialization and stays valid for the life of the
/// process.
#[unsafe(no_mangle)]
pub extern "C" fn scalardb_analytics_connector_static_information()
-> ScalardbAnalyticsConnectorString {
    static TEXT: OnceLock<TerminatedText> = OnceLock::new();
    TEXT.get_or_init(|| TerminatedText::new(registry::static_information()))
        .view()
}

/// Initializes the library once for the process.
///
/// Initializing an initialized library returns `invalid state`.
///
/// # Safety
///
/// `settings` points to a valid settings structure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_library_initialize(
    settings: *const ScalardbAnalyticsConnectorLibrarySettings,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes a valid settings pointer or null.
        let settings = unsafe { reference(settings, "settings") }?;
        // SAFETY: the caller passes a NUL-terminated string or null.
        let directory = unsafe {
            read_optional_text(settings.adbc_driver_directory, "the ADBC driver directory")
        }?;
        library::initialize(LibrarySettings {
            adbc_driver_directory: directory.map(PathBuf::from),
        })
    })
}

/// Shuts the library down. Data Sources still open return `invalid state`.
#[unsafe(no_mangle)]
pub extern "C" fn scalardb_analytics_connector_library_shutdown()
-> *mut ScalardbAnalyticsConnectorError {
    contain(library::shutdown)
}

/// Opens a Data Source of the named Connector.
///
/// The options are validated against the connector's static information;
/// invalid options return `invalid settings` naming the option. The connection
/// timeout limits connecting while the Data Source and its sessions open.
///
/// # Safety
///
/// `connector` and every option string are valid strings, `options` points to
/// `option_count` options, and `out_data_source` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_data_source_open(
    connector: *const c_char,
    options: *const ScalardbAnalyticsConnectorOption,
    option_count: usize,
    connection_timeout_milliseconds: u64,
    out_data_source: *mut *mut ScalardbAnalyticsConnectorDataSource,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes NUL-terminated strings and an options array.
        let (connector, options) = unsafe {
            let connector = read_text(connector, "the Connector name")?;
            let options = read_slice(options, option_count, "the options")?
                .iter()
                .map(|option| {
                    Ok((
                        read_text(option.key, "an option key")?,
                        read_secret_text(option.value, "an option value")?,
                    ))
                })
                .collect::<Result<Vec<_>, ConnectorError>>()?;
            (connector, options)
        };
        let data_source = library::current()?.open_data_source(
            &connector,
            options,
            timeout(connection_timeout_milliseconds),
        )?;
        let handle = Box::new(ScalardbAnalyticsConnectorDataSource { state: data_source });
        // SAFETY: the caller passes a writable output pointer.
        unsafe { write(out_data_source, Box::into_raw(handle), "out_data_source") }
    })
}

/// Closes a Data Source handle. Its sessions' later calls return `invalid state`.
///
/// # Safety
///
/// `data_source` is null or an open handle, not used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_data_source_close(
    data_source: *mut ScalardbAnalyticsConnectorDataSource,
) {
    // SAFETY: the caller passes an open handle or null.
    unsafe { close(data_source, |handle| handle.state.close()) }
}

/// Opens a session on a Data Source.
///
/// # Safety
///
/// `data_source` is an open handle and `out_session` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_session_open(
    data_source: *const ScalardbAnalyticsConnectorDataSource,
    out_session: *mut *mut ScalardbAnalyticsConnectorSession,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes an open handle.
        let data_source = unsafe { reference(data_source, "data_source") }?;
        let session = data_source.state.open_session()?;
        let handle = Box::new(ScalardbAnalyticsConnectorSession { state: session });
        // SAFETY: the caller passes a writable output pointer.
        unsafe { write(out_session, Box::into_raw(handle), "out_session") }
    })
}

/// Closes a session handle. Its operations' later calls return `invalid state`.
///
/// # Safety
///
/// `session` is null or an open handle, not used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_session_close(
    session: *mut ScalardbAnalyticsConnectorSession,
) {
    // SAFETY: the caller passes an open handle or null.
    unsafe { close(session, |handle| handle.state.close()) }
}

/// Plans one pushdown request: a complete offered Substrait plan as protobuf bytes.
///
/// Planning may use the session's connection, so a session running another
/// operation returns `invalid state`.
///
/// # Safety
///
/// `session` is an open handle, `offered_plan` is readable, and `out_result`
/// is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_plan_pushdown(
    session: *const ScalardbAnalyticsConnectorSession,
    offered_plan: ScalardbAnalyticsConnectorBytes,
    out_result: *mut *mut ScalardbAnalyticsConnectorPushdownResult,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes an open handle.
        let session = &unsafe { reference(session, "session") }?.state;
        // SAFETY: the caller passes readable bytes.
        let offered_plan =
            unsafe { read_bytes(offered_plan.data, offered_plan.length, "the offered plan") }?;
        let outcome = session.plan_pushdown(offered_plan)?;
        let unsupported_reason = match &outcome {
            PlanningOutcome::Unsupported { reason } => Some(TerminatedText::new(reason)),
            PlanningOutcome::Planned { .. } => None,
        };
        let handle = Box::new(ScalardbAnalyticsConnectorPushdownResult {
            outcome,
            unsupported_reason,
        });
        // SAFETY: the caller passes a writable output pointer.
        unsafe { write(out_result, Box::into_raw(handle), "out_result") }
    })
}

/// Returns whether the Connector planned the request.
///
/// # Safety
///
/// `result` is a live pushdown result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_pushdown_result_is_planned(
    result: *const ScalardbAnalyticsConnectorPushdownResult,
) -> bool {
    // SAFETY: the caller passes a live result.
    unsafe { result.as_ref() }
        .is_some_and(|result| matches!(result.outcome, PlanningOutcome::Planned { .. }))
}

/// Returns why the Connector could not plan the request; absent when it did.
///
/// # Safety
///
/// `result` is a live pushdown result; the string lives as long as it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_pushdown_result_unsupported_reason(
    result: *const ScalardbAnalyticsConnectorPushdownResult,
) -> ScalardbAnalyticsConnectorString {
    // SAFETY: the caller passes a live result.
    unsafe { result.as_ref() }
        .and_then(|result| result.unsupported_reason.as_ref())
        .map_or(
            ScalardbAnalyticsConnectorString::ABSENT,
            TerminatedText::view,
        )
}

/// Returns the remote Substrait plan as protobuf bytes; absent when not planned.
///
/// # Safety
///
/// `result` is a live pushdown result; the bytes live as long as it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_pushdown_result_remote_plan(
    result: *const ScalardbAnalyticsConnectorPushdownResult,
) -> ScalardbAnalyticsConnectorBytes {
    // SAFETY: the caller passes a live result.
    match unsafe { result.as_ref() }.map(|result| &result.outcome) {
        Some(PlanningOutcome::Planned { remote_plan, .. }) => {
            ScalardbAnalyticsConnectorBytes::borrowed(remote_plan)
        }
        _ => ScalardbAnalyticsConnectorBytes::ABSENT,
    }
}

/// Returns the local Substrait plan as protobuf bytes; absent when no local
/// work remains or the request was not planned.
///
/// # Safety
///
/// `result` is a live pushdown result; the bytes live as long as it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_pushdown_result_local_plan(
    result: *const ScalardbAnalyticsConnectorPushdownResult,
) -> ScalardbAnalyticsConnectorBytes {
    // SAFETY: the caller passes a live result.
    match unsafe { result.as_ref() }.map(|result| &result.outcome) {
        Some(PlanningOutcome::Planned {
            local_plan: Some(local_plan),
            ..
        }) => ScalardbAnalyticsConnectorBytes::borrowed(local_plan),
        _ => ScalardbAnalyticsConnectorBytes::ABSENT,
    }
}

/// Returns the remote execution of a planned result as a new handle.
///
/// An unplanned result returns `invalid request`.
///
/// # Safety
///
/// `result` is a live pushdown result and `out_remote_execution` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_pushdown_result_remote_execution(
    result: *const ScalardbAnalyticsConnectorPushdownResult,
    out_remote_execution: *mut *mut ScalardbAnalyticsConnectorRemoteExecution,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes a live result.
        let result = unsafe { reference(result, "result") }?;
        let PlanningOutcome::Planned { remote, .. } = &result.outcome else {
            return Err(invalid_request("the pushdown was not planned"));
        };
        let handle = Box::new(ScalardbAnalyticsConnectorRemoteExecution {
            remote: remote.clone(),
        });
        // SAFETY: the caller passes a writable output pointer.
        unsafe {
            write(
                out_remote_execution,
                Box::into_raw(handle),
                "out_remote_execution",
            )
        }
    })
}

/// Frees a pushdown result.
///
/// # Safety
///
/// `result` is null or a live pushdown result, not used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_pushdown_result_free(
    result: *mut ScalardbAnalyticsConnectorPushdownResult,
) {
    // SAFETY: the caller passes a live result or null.
    unsafe { close(result, |_| ()) }
}

/// Encodes a remote execution as opaque bytes for another process.
///
/// The bytes carry no credentials and are valid only for this library version.
///
/// # Safety
///
/// `remote_execution` is a live handle and `out_bytes` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_remote_execution_encode(
    remote_execution: *const ScalardbAnalyticsConnectorRemoteExecution,
    out_bytes: *mut ScalardbAnalyticsConnectorOwnedBytes,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes a live handle.
        let remote_execution = unsafe { reference(remote_execution, "remote_execution") }?;
        let bytes = remote_execution.remote.to_bytes().into_boxed_slice();
        let length = bytes.len();
        let data = Box::into_raw(bytes).cast::<u8>();
        // SAFETY: the caller passes a writable output pointer.
        unsafe {
            write(
                out_bytes,
                ScalardbAnalyticsConnectorOwnedBytes { data, length },
                "out_bytes",
            )
        }
    })
}

/// Restores a remote execution from bytes encoded by
/// `scalardb_analytics_connector_remote_execution_encode`.
///
/// Bytes that cannot be restored return `invalid request`.
///
/// # Safety
///
/// `session` is an open handle, `bytes` is readable, and
/// `out_remote_execution` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_remote_execution_restore(
    session: *const ScalardbAnalyticsConnectorSession,
    bytes: ScalardbAnalyticsConnectorBytes,
    out_remote_execution: *mut *mut ScalardbAnalyticsConnectorRemoteExecution,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes an open handle.
        let session = &unsafe { reference(session, "session") }?.state;
        // SAFETY: the caller passes readable bytes.
        let bytes = unsafe { read_bytes(bytes.data, bytes.length, "the remote execution") }?;
        let remote = session.restore_remote_execution(bytes)?;
        let handle = Box::new(ScalardbAnalyticsConnectorRemoteExecution { remote });
        // SAFETY: the caller passes a writable output pointer.
        unsafe {
            write(
                out_remote_execution,
                Box::into_raw(handle),
                "out_remote_execution",
            )
        }
    })
}

/// Frees a remote execution handle.
///
/// # Safety
///
/// `remote_execution` is null or a live handle, not used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_remote_execution_free(
    remote_execution: *mut ScalardbAnalyticsConnectorRemoteExecution,
) {
    // SAFETY: the caller passes a live handle or null.
    unsafe { close(remote_execution, |_| ()) }
}

/// Starts a read of a remote execution and returns its operation and stream.
///
/// The query is sent at the stream's first pull, and the timeout counts from
/// there. A session running another operation returns `invalid state` and
/// leaves that operation running. The stream belongs to the caller, who
/// releases it through its `release` callback.
///
/// # Safety
///
/// `session` and `remote_execution` are live handles, and `out_operation` and
/// `out_stream` are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_read_start(
    session: *const ScalardbAnalyticsConnectorSession,
    remote_execution: *const ScalardbAnalyticsConnectorRemoteExecution,
    timeout_milliseconds: u64,
    out_operation: *mut *mut ScalardbAnalyticsConnectorOperation,
    out_stream: *mut FFI_ArrowArrayStream,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes an open handle.
        let session = &unsafe { reference(session, "session") }?.state;
        // SAFETY: the caller passes a live handle.
        let remote_execution = unsafe { reference(remote_execution, "remote_execution") }?;
        if out_operation.is_null() || out_stream.is_null() {
            return Err(invalid_request("an output pointer is null"));
        }
        let operation = session.start_read(
            remote_execution.remote.clone(),
            timeout(timeout_milliseconds),
        )?;
        let stream = stream::export(Arc::clone(&operation));
        let handle = Box::new(ScalardbAnalyticsConnectorOperation {
            operation,
            error: OnceLock::new(),
        });
        // SAFETY: both output pointers are non-null and the caller passes
        // writable memory.
        unsafe {
            out_stream.write(stream);
            out_operation.write(Box::into_raw(handle));
        }
        Ok(())
    })
}

/// Cancels an operation. It may be called from any thread.
///
/// The stream's next pull, or the pull in progress, ends with `cancellation`.
///
/// # Safety
///
/// `operation` is an open handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_operation_cancel(
    operation: *const ScalardbAnalyticsConnectorOperation,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes an open handle.
        unsafe { reference(operation, "operation") }?
            .operation
            .cancel()
    })
}

/// Returns the error that ended the operation's stream, or null.
///
/// The error stays valid until the operation is closed and is not freed by
/// the caller.
///
/// # Safety
///
/// `operation` is an open handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_operation_error(
    operation: *const ScalardbAnalyticsConnectorOperation,
) -> *const ScalardbAnalyticsConnectorError {
    // SAFETY: the caller passes an open handle or null.
    let Some(handle) = (unsafe { operation.as_ref() }) else {
        return ptr::null();
    };
    match handle.operation.error() {
        Some(error) => handle
            .error
            .get_or_init(|| OwnedError::new(error.clone()))
            .view(),
        None => ptr::null(),
    }
}

/// Closes an operation, releasing every Data Source resource it holds.
///
/// A stream still held by the caller returns `invalid state` on its next pull.
///
/// # Safety
///
/// `operation` is null or an open handle, not used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_operation_close(
    operation: *mut ScalardbAnalyticsConnectorOperation,
) {
    // SAFETY: the caller passes an open handle or null.
    unsafe { close(operation, |handle| handle.operation.close()) }
}

/// Frees an error returned by a function.
///
/// # Safety
///
/// `error` is null or an error returned by a function of this library and not
/// yet freed; an operation's error is not freed with this function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_error_free(
    error: *mut ScalardbAnalyticsConnectorError,
) {
    // SAFETY: forwarded from the caller.
    unsafe { OwnedError::free(error) }
}

/// Frees bytes returned by this library.
///
/// # Safety
///
/// `bytes` was returned by this library and not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn scalardb_analytics_connector_owned_bytes_free(
    bytes: ScalardbAnalyticsConnectorOwnedBytes,
) {
    if !bytes.data.is_null() {
        // SAFETY: the bytes were leaked from a boxed slice of this length.
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(bytes.data, bytes.length)) });
    }
}

/// Runs `call` and converts its outcome into the C error convention.
///
/// The runtime already contains connector panics and marks the affected
/// session; this guard keeps any remaining panic from crossing the C ABI.
fn contain(
    call: impl FnOnce() -> Result<(), ConnectorError>,
) -> *mut ScalardbAnalyticsConnectorError {
    match panic::catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(())) => ptr::null_mut(),
        Ok(Err(error)) => OwnedError::into_raw(error),
        Err(_) => OwnedError::into_raw(panic_error()),
    }
}

/// Runs `before` on a handle and frees it, containing any panic.
///
/// # Safety
///
/// `handle` is null or was created by `Box::into_raw` in this module and is
/// not used after this call.
unsafe fn close<T>(handle: *mut T, before: impl FnOnce(&T)) {
    if handle.is_null() {
        return;
    }
    // SAFETY: forwarded from the caller.
    let handle = unsafe { Box::from_raw(handle) };
    // A close has no error to return; a panic while releasing is contained so
    // that it does not cross the C ABI.
    let _ = panic::catch_unwind(AssertUnwindSafe(move || {
        before(&handle);
        drop(handle);
    }));
}

/// Borrows a required input pointer.
///
/// # Safety
///
/// A non-null `pointer` points to a live `T`.
unsafe fn reference<'a, T>(pointer: *const T, name: &str) -> Result<&'a T, ConnectorError> {
    // SAFETY: forwarded from the caller.
    unsafe { pointer.as_ref() }.ok_or_else(|| invalid_request(&format!("{name} is null")))
}

/// Writes a required output pointer.
///
/// # Safety
///
/// A non-null `pointer` is writable.
unsafe fn write<T>(pointer: *mut T, value: T, name: &str) -> Result<(), ConnectorError> {
    if pointer.is_null() {
        return Err(invalid_request(&format!("{name} is null")));
    }
    // SAFETY: forwarded from the caller.
    unsafe { pointer.write(value) };
    Ok(())
}

/// Borrows an input array.
///
/// # Safety
///
/// A non-null `data` points to `length` readable values.
unsafe fn read_slice<'a, T>(
    data: *const T,
    length: usize,
    name: &str,
) -> Result<&'a [T], ConnectorError> {
    if data.is_null() {
        return if length == 0 {
            Ok(&[])
        } else {
            Err(invalid_request(&format!("{name} is null")))
        };
    }
    // SAFETY: forwarded from the caller.
    Ok(unsafe { slice::from_raw_parts(data, length) })
}

/// Borrows input bytes.
///
/// # Safety
///
/// A non-null `data` points to `length` readable bytes.
unsafe fn read_bytes<'a>(
    data: *const u8,
    length: usize,
    name: &str,
) -> Result<&'a [u8], ConnectorError> {
    // SAFETY: forwarded from the caller.
    unsafe { read_slice(data, length, name) }
}

fn timeout(milliseconds: u64) -> Option<Duration> {
    (milliseconds != 0).then(|| Duration::from_millis(milliseconds))
}

fn invalid_request(message: &str) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidRequest, false, message)
}
