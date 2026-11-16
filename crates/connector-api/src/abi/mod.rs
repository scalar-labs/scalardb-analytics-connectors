//! The connector API's C functions and types.
//!
//! This module is the only place that handles raw pointers. Each function
//! checks its pointer arguments, converts them to safe values, calls the
//! runtime, and contains any panic so that none crosses the C ABI.
//!
//! The library exports one symbol, `scalardb_analytics_connector_api`, which
//! returns [`ScalardbAnalyticsConnectorApi`]. Each other function is reached
//! through the field of the same name, which documents it.
//!
//! Conventions shared by every function:
//!
//! - A function that can fail returns null on success, or an error the caller
//!   frees with `error_free`.
//! - Input text is a NUL-terminated UTF-8 `const char*`.
//! - Output text is UTF-8 with a length and a NUL terminator after it; absent
//!   output text has a null `data`.
//! - Text inside structures, in both directions, is a
//!   `ScalardbAnalyticsConnectorString`, so an output structure can be passed
//!   back unchanged as an input.
//! - Binary data, such as Substrait plans, carries a length and no terminator.
//! - The library copies every input during the call.
//! - A timeout of 0 milliseconds means no limit.
//! - An output structure tree passes to the caller, which frees it once with
//!   the free function named for its root; that frees the whole tree.

mod comparison;
mod error;
mod metadata;
mod owned;
mod stream;
mod text;

pub use comparison::*;
pub use error::{
    ScalardbAnalyticsConnectorDiagnostic, ScalardbAnalyticsConnectorDiagnosticPayload,
    ScalardbAnalyticsConnectorDiagnosticType, ScalardbAnalyticsConnectorError,
    ScalardbAnalyticsConnectorErrorKind,
};
pub use metadata::*;
pub use text::ScalardbAnalyticsConnectorString;

use std::{
    collections::BTreeSet,
    ffi::c_char,
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    ptr, slice,
    sync::{Arc, OnceLock},
    time::Duration,
};

use arrow_array::ffi_stream::FFI_ArrowArrayStream;
use connector_sdk::{
    ConnectorError, ErrorKind, LibrarySettings, NameFilter,
    runtime::{
        Cancellation, DataSourceState, Operation, PlanningOutcome, SessionState,
        SharedRemoteExecution, panic_error,
    },
};

use crate::{library, registry};
use error::OwnedError;
use metadata::{read_source_metadata, read_table_metadata, read_table_reference};
use owned::{Arena, Owned};
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

/// Bytes owned by the caller, freed with `owned_bytes_free`.
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

/// A set of names matched exactly, without case folding or patterns.
///
/// A metadata request takes a pointer to one; a null pointer matches every
/// name, and a filter with no names matches none.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorNameFilter {
    pub names: *const *const c_char,
    pub name_count: usize,
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

/// A caller's means of cancelling metadata requests from any thread.
pub struct ScalardbAnalyticsConnectorCancellation {
    cancellation: Cancellation,
}

/// A read operation. It may be cancelled from any thread.
pub struct ScalardbAnalyticsConnectorOperation {
    operation: Arc<Operation>,
    /// The C view of the operation's error, built on first retrieval.
    error: OnceLock<Box<OwnedError>>,
}

/// The library's functions, returned by `scalardb_analytics_connector_api`.
#[repr(C)]
pub struct ScalardbAnalyticsConnectorApi {
    /// Returns the static information JSON document.
    ///
    /// It is available before initialization and stays valid for the life of the
    /// process.
    pub static_information: extern "C" fn() -> ScalardbAnalyticsConnectorString,
    /// Initializes the library once for the process.
    ///
    /// Initializing an initialized library returns `invalid state`.
    ///
    /// # Safety
    ///
    /// `settings` points to a valid settings structure.
    pub library_initialize: unsafe extern "C" fn(
        settings: *const ScalardbAnalyticsConnectorLibrarySettings,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Shuts the library down. Data Sources still open return `invalid state`.
    pub library_shutdown: extern "C" fn() -> *mut ScalardbAnalyticsConnectorError,
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
    pub data_source_open: unsafe extern "C" fn(
        connector: *const c_char,
        options: *const ScalardbAnalyticsConnectorOption,
        option_count: usize,
        connection_timeout_milliseconds: u64,
        out_data_source: *mut *mut ScalardbAnalyticsConnectorDataSource,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Closes a Data Source handle. Its sessions' later calls return `invalid state`.
    ///
    /// # Safety
    ///
    /// `data_source` is null or an open handle, not used after this call.
    pub data_source_close:
        unsafe extern "C" fn(data_source: *mut ScalardbAnalyticsConnectorDataSource),
    /// Opens a session on a Data Source.
    ///
    /// # Safety
    ///
    /// `data_source` is an open handle and `out_session` is writable.
    pub session_open: unsafe extern "C" fn(
        data_source: *const ScalardbAnalyticsConnectorDataSource,
        out_session: *mut *mut ScalardbAnalyticsConnectorSession,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Closes a session handle. Its operations' later calls return `invalid state`.
    ///
    /// # Safety
    ///
    /// `session` is null or an open handle, not used after this call.
    pub session_close: unsafe extern "C" fn(session: *mut ScalardbAnalyticsConnectorSession),
    /// Plans one pushdown request: a complete offered Substrait plan as protobuf bytes.
    ///
    /// Planning may use the session's connection, so a session running another
    /// operation returns `invalid state`.
    ///
    /// # Safety
    ///
    /// `session` is an open handle, `offered_plan` is readable, and `out_result`
    /// is writable.
    pub plan_pushdown: unsafe extern "C" fn(
        session: *const ScalardbAnalyticsConnectorSession,
        offered_plan: ScalardbAnalyticsConnectorBytes,
        out_result: *mut *mut ScalardbAnalyticsConnectorPushdownResult,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Returns whether the Connector planned the request.
    ///
    /// # Safety
    ///
    /// `result` is a live pushdown result.
    pub pushdown_result_is_planned:
        unsafe extern "C" fn(result: *const ScalardbAnalyticsConnectorPushdownResult) -> bool,
    /// Returns why the Connector could not plan the request; absent when it did.
    ///
    /// # Safety
    ///
    /// `result` is a live pushdown result; the string lives as long as it.
    pub pushdown_result_unsupported_reason:
        unsafe extern "C" fn(
            result: *const ScalardbAnalyticsConnectorPushdownResult,
        ) -> ScalardbAnalyticsConnectorString,
    /// Returns the remote Substrait plan as protobuf bytes; absent when not planned.
    ///
    /// # Safety
    ///
    /// `result` is a live pushdown result; the bytes live as long as it.
    pub pushdown_result_remote_plan: unsafe extern "C" fn(
        result: *const ScalardbAnalyticsConnectorPushdownResult,
    ) -> ScalardbAnalyticsConnectorBytes,
    /// Returns the local Substrait plan as protobuf bytes; absent when no local
    /// work remains or the request was not planned.
    ///
    /// # Safety
    ///
    /// `result` is a live pushdown result; the bytes live as long as it.
    pub pushdown_result_local_plan: unsafe extern "C" fn(
        result: *const ScalardbAnalyticsConnectorPushdownResult,
    ) -> ScalardbAnalyticsConnectorBytes,
    /// Returns the remote execution of a planned result as a new handle.
    ///
    /// An unplanned result returns `invalid request`.
    ///
    /// # Safety
    ///
    /// `result` is a live pushdown result and `out_remote_execution` is writable.
    pub pushdown_result_remote_execution:
        unsafe extern "C" fn(
            result: *const ScalardbAnalyticsConnectorPushdownResult,
            out_remote_execution: *mut *mut ScalardbAnalyticsConnectorRemoteExecution,
        ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees a pushdown result.
    ///
    /// # Safety
    ///
    /// `result` is null or a live pushdown result, not used after this call.
    pub pushdown_result_free:
        unsafe extern "C" fn(result: *mut ScalardbAnalyticsConnectorPushdownResult),
    /// Encodes a remote execution as opaque bytes for another process.
    ///
    /// The bytes carry no credentials and are valid only for this library version.
    ///
    /// # Safety
    ///
    /// `remote_execution` is a live handle and `out_bytes` is writable.
    pub remote_execution_encode: unsafe extern "C" fn(
        remote_execution: *const ScalardbAnalyticsConnectorRemoteExecution,
        out_bytes: *mut ScalardbAnalyticsConnectorOwnedBytes,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Restores a remote execution from bytes encoded by
    /// `remote_execution_encode`.
    ///
    /// Bytes that cannot be restored return `invalid request`.
    ///
    /// # Safety
    ///
    /// `session` is an open handle, `bytes` is readable, and
    /// `out_remote_execution` is writable.
    pub remote_execution_restore: unsafe extern "C" fn(
        session: *const ScalardbAnalyticsConnectorSession,
        bytes: ScalardbAnalyticsConnectorBytes,
        out_remote_execution: *mut *mut ScalardbAnalyticsConnectorRemoteExecution,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees a remote execution handle.
    ///
    /// # Safety
    ///
    /// `remote_execution` is null or a live handle, not used after this call.
    pub remote_execution_free:
        unsafe extern "C" fn(remote_execution: *mut ScalardbAnalyticsConnectorRemoteExecution),
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
    pub read_start: unsafe extern "C" fn(
        session: *const ScalardbAnalyticsConnectorSession,
        remote_execution: *const ScalardbAnalyticsConnectorRemoteExecution,
        timeout_milliseconds: u64,
        out_operation: *mut *mut ScalardbAnalyticsConnectorOperation,
        out_stream: *mut FFI_ArrowArrayStream,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Creates a cancellation for metadata requests, freed with
    /// `cancellation_free`.
    ///
    /// # Safety
    ///
    /// `out_cancellation` is writable.
    pub cancellation_create: unsafe extern "C" fn(
        out_cancellation: *mut *mut ScalardbAnalyticsConnectorCancellation,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Cancels every metadata request running with the cancellation, from any
    /// thread, and every later request given it: they return `cancellation`.
    ///
    /// # Safety
    ///
    /// `cancellation` is a live handle.
    pub cancellation_cancel: unsafe extern "C" fn(
        cancellation: *const ScalardbAnalyticsConnectorCancellation,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees a cancellation. No request may be running with it.
    ///
    /// # Safety
    ///
    /// `cancellation` is null or a live handle, not used after this call.
    pub cancellation_free:
        unsafe extern "C" fn(cancellation: *mut ScalardbAnalyticsConnectorCancellation),
    /// Lists the namespaces whose names exactly match `namespaces`, or all of
    /// them when it is null.
    ///
    /// Metadata requests return their whole result when the call returns. The
    /// timeout counts from the call, and `cancellation`, which may be null, stops
    /// the call from another thread. A failure returns only the error. A session
    /// running a read returns `invalid state`.
    ///
    /// # Safety
    ///
    /// `session` is an open handle, `namespaces` is null or a valid filter,
    /// `cancellation` is null or a live handle, and `out_list` is writable.
    pub namespace_list: unsafe extern "C" fn(
        session: *const ScalardbAnalyticsConnectorSession,
        namespaces: *const ScalardbAnalyticsConnectorNameFilter,
        timeout_milliseconds: u64,
        cancellation: *const ScalardbAnalyticsConnectorCancellation,
        out_list: *mut *mut ScalardbAnalyticsConnectorNamespaceList,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees a namespace list.
    ///
    /// # Safety
    ///
    /// `list` is null or a live result, not used after this call.
    pub namespace_list_free:
        unsafe extern "C" fn(list: *mut ScalardbAnalyticsConnectorNamespaceList),
    /// Lists the tables of the matching namespaces whose names exactly match
    /// `tables`. A null filter matches every name. Otherwise as
    /// `namespace_list`.
    ///
    /// # Safety
    ///
    /// `session` is an open handle, `namespaces` and `tables` are null or valid
    /// filters, `cancellation` is null or a live handle, and `out_list` is
    /// writable.
    pub table_list: unsafe extern "C" fn(
        session: *const ScalardbAnalyticsConnectorSession,
        namespaces: *const ScalardbAnalyticsConnectorNameFilter,
        tables: *const ScalardbAnalyticsConnectorNameFilter,
        timeout_milliseconds: u64,
        cancellation: *const ScalardbAnalyticsConnectorCancellation,
        out_list: *mut *mut ScalardbAnalyticsConnectorTableList,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees a table list.
    ///
    /// # Safety
    ///
    /// `list` is null or a live result, not used after this call.
    pub table_list_free: unsafe extern "C" fn(list: *mut ScalardbAnalyticsConnectorTableList),
    /// Reads one record per requested table, in request order, including tables
    /// that do not exist or could not be read. Otherwise as
    /// `namespace_list`.
    ///
    /// A record's `table_metadata` and `source_metadata` are passed unchanged to
    /// `table_metadata_compare` and
    /// `source_metadata_compare`.
    ///
    /// # Safety
    ///
    /// `session` is an open handle, `tables` points to `table_count` valid name
    /// sequences, `cancellation` is null or a live handle, and `out_result` is
    /// writable.
    pub table_metadata: unsafe extern "C" fn(
        session: *const ScalardbAnalyticsConnectorSession,
        tables: *const ScalardbAnalyticsConnectorNames,
        table_count: usize,
        timeout_milliseconds: u64,
        cancellation: *const ScalardbAnalyticsConnectorCancellation,
        out_result: *mut *mut ScalardbAnalyticsConnectorTableMetadataResult,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees a table metadata result.
    ///
    /// # Safety
    ///
    /// `result` is null or a live result, not used after this call.
    pub table_metadata_result_free:
        unsafe extern "C" fn(result: *mut ScalardbAnalyticsConnectorTableMetadataResult),
    /// Compares two observations of one table's table metadata that the caller
    /// paired, such as a stored one and a newly retrieved one.
    ///
    /// Only values observed on both sides are compared. `out_differences`
    /// receives null when no value differs. Input that is not valid table
    /// metadata is `invalid request`. It needs no handle.
    ///
    /// # Safety
    ///
    /// `before` and `after` point to valid table metadata, and `out_differences`
    /// is writable.
    pub table_metadata_compare: unsafe extern "C" fn(
        before: *const ScalardbAnalyticsConnectorTableMetadata,
        after: *const ScalardbAnalyticsConnectorTableMetadata,
        out_differences: *mut *mut ScalardbAnalyticsConnectorTableMetadataDifferences,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees table metadata differences.
    ///
    /// # Safety
    ///
    /// `differences` is null or a live result, not used after this call.
    pub table_metadata_differences_free:
        unsafe extern "C" fn(differences: *mut ScalardbAnalyticsConnectorTableMetadataDifferences),
    /// Compares two observations of one table's source metadata as JSON
    /// documents, through the connector that produced them, which restores
    /// both. Their table metadata is compared with `table_metadata_compare`.
    ///
    /// `out_differences` receives null when the documents do not differ.
    /// Documents of different connectors, of a connector not built into the
    /// library, or that the connector cannot restore, such as one stored
    /// before a connector update changed its shape, are `invalid request`. It
    /// needs an initialized library and no handle.
    ///
    /// # Safety
    ///
    /// `before` and `after` point to valid source metadata, and `out_differences`
    /// is writable.
    pub source_metadata_compare: unsafe extern "C" fn(
        before: *const ScalardbAnalyticsConnectorSourceMetadata,
        after: *const ScalardbAnalyticsConnectorSourceMetadata,
        out_differences: *mut *mut ScalardbAnalyticsConnectorSourceMetadataDifferences,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Frees source metadata differences.
    ///
    /// # Safety
    ///
    /// `differences` is null or a live result, not used after this call.
    pub source_metadata_differences_free:
        unsafe extern "C" fn(differences: *mut ScalardbAnalyticsConnectorSourceMetadataDifferences),
    /// Cancels an operation. It may be called from any thread.
    ///
    /// The stream's next pull, or the pull in progress, ends with `cancellation`.
    ///
    /// # Safety
    ///
    /// `operation` is an open handle.
    pub operation_cancel: unsafe extern "C" fn(
        operation: *const ScalardbAnalyticsConnectorOperation,
    ) -> *mut ScalardbAnalyticsConnectorError,
    /// Returns the error that ended the operation's stream, or null.
    ///
    /// The error stays valid until the operation is closed and is not freed by
    /// the caller.
    ///
    /// # Safety
    ///
    /// `operation` is an open handle.
    pub operation_error: unsafe extern "C" fn(
        operation: *const ScalardbAnalyticsConnectorOperation,
    ) -> *const ScalardbAnalyticsConnectorError,
    /// Closes an operation, releasing every Data Source resource it holds.
    ///
    /// A stream still held by the caller returns `invalid state` on its next pull.
    ///
    /// # Safety
    ///
    /// `operation` is null or an open handle, not used after this call.
    pub operation_close: unsafe extern "C" fn(operation: *mut ScalardbAnalyticsConnectorOperation),
    /// Frees an error returned by a function.
    ///
    /// # Safety
    ///
    /// `error` is null or an error returned by a function of this library and not
    /// yet freed; an operation's error is not freed with this function.
    pub error_free: unsafe extern "C" fn(error: *mut ScalardbAnalyticsConnectorError),
    /// Frees bytes returned by this library.
    ///
    /// # Safety
    ///
    /// `bytes` was returned by this library and not yet freed.
    pub owned_bytes_free: unsafe extern "C" fn(bytes: ScalardbAnalyticsConnectorOwnedBytes),
    /// Returns whether the library is initialized in this process.
    ///
    /// A caller that does not own the library, such as an adapter loaded into
    /// an application that initialized it, uses the library as it is when this
    /// returns true, and initializes it only when this returns false.
    pub library_is_initialized: extern "C" fn() -> bool,
}

static API: ScalardbAnalyticsConnectorApi = ScalardbAnalyticsConnectorApi {
    static_information,
    library_initialize,
    library_shutdown,
    data_source_open,
    data_source_close,
    session_open,
    session_close,
    plan_pushdown,
    pushdown_result_is_planned,
    pushdown_result_unsupported_reason,
    pushdown_result_remote_plan,
    pushdown_result_local_plan,
    pushdown_result_remote_execution,
    pushdown_result_free,
    remote_execution_encode,
    remote_execution_restore,
    remote_execution_free,
    read_start,
    cancellation_create,
    cancellation_cancel,
    cancellation_free,
    namespace_list,
    namespace_list_free,
    table_list,
    table_list_free,
    table_metadata,
    table_metadata_result_free,
    table_metadata_compare,
    table_metadata_differences_free,
    source_metadata_compare,
    source_metadata_differences_free,
    operation_cancel,
    operation_error,
    operation_close,
    error_free,
    owned_bytes_free,
    library_is_initialized,
};

/// Returns the library's functions. It is the only exported symbol, is
/// available before initialization, and stays valid for the life of the process.
#[unsafe(no_mangle)]
pub extern "C" fn scalardb_analytics_connector_api() -> &'static ScalardbAnalyticsConnectorApi {
    &API
}

extern "C" fn static_information() -> ScalardbAnalyticsConnectorString {
    static TEXT: OnceLock<TerminatedText> = OnceLock::new();
    TEXT.get_or_init(|| TerminatedText::new(registry::static_information()))
        .view()
}

unsafe extern "C" fn library_initialize(
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

extern "C" fn library_shutdown() -> *mut ScalardbAnalyticsConnectorError {
    contain(library::shutdown)
}

extern "C" fn library_is_initialized() -> bool {
    library::is_initialized()
}

unsafe extern "C" fn data_source_open(
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

unsafe extern "C" fn data_source_close(data_source: *mut ScalardbAnalyticsConnectorDataSource) {
    // SAFETY: the caller passes an open handle or null.
    unsafe { close(data_source, |handle| handle.state.close()) }
}

unsafe extern "C" fn session_open(
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

unsafe extern "C" fn session_close(session: *mut ScalardbAnalyticsConnectorSession) {
    // SAFETY: the caller passes an open handle or null.
    unsafe { close(session, |handle| handle.state.close()) }
}

unsafe extern "C" fn plan_pushdown(
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

unsafe extern "C" fn pushdown_result_is_planned(
    result: *const ScalardbAnalyticsConnectorPushdownResult,
) -> bool {
    // SAFETY: the caller passes a live result.
    unsafe { result.as_ref() }
        .is_some_and(|result| matches!(result.outcome, PlanningOutcome::Planned { .. }))
}

unsafe extern "C" fn pushdown_result_unsupported_reason(
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

unsafe extern "C" fn pushdown_result_remote_plan(
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

unsafe extern "C" fn pushdown_result_local_plan(
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

unsafe extern "C" fn pushdown_result_remote_execution(
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

unsafe extern "C" fn pushdown_result_free(result: *mut ScalardbAnalyticsConnectorPushdownResult) {
    // SAFETY: the caller passes a live result or null.
    unsafe { close(result, |_| ()) }
}

unsafe extern "C" fn remote_execution_encode(
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

unsafe extern "C" fn remote_execution_restore(
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

unsafe extern "C" fn remote_execution_free(
    remote_execution: *mut ScalardbAnalyticsConnectorRemoteExecution,
) {
    // SAFETY: the caller passes a live handle or null.
    unsafe { close(remote_execution, |_| ()) }
}

unsafe extern "C" fn read_start(
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
        // SAFETY: both output pointers are non-null and the caller passes
        // writable memory.
        unsafe { write_operation(operation, out_operation, out_stream) };
        Ok(())
    })
}

unsafe extern "C" fn cancellation_create(
    out_cancellation: *mut *mut ScalardbAnalyticsConnectorCancellation,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        let handle = Box::new(ScalardbAnalyticsConnectorCancellation {
            cancellation: Cancellation::new(),
        });
        // SAFETY: the caller passes a writable output pointer.
        unsafe { write(out_cancellation, Box::into_raw(handle), "out_cancellation") }
    })
}

unsafe extern "C" fn cancellation_cancel(
    cancellation: *const ScalardbAnalyticsConnectorCancellation,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes a live handle.
        unsafe { reference(cancellation, "cancellation") }?
            .cancellation
            .cancel()
    })
}

unsafe extern "C" fn cancellation_free(cancellation: *mut ScalardbAnalyticsConnectorCancellation) {
    // SAFETY: the caller passes a live handle or null.
    unsafe { close(cancellation, |_| ()) }
}

unsafe extern "C" fn namespace_list(
    session: *const ScalardbAnalyticsConnectorSession,
    namespaces: *const ScalardbAnalyticsConnectorNameFilter,
    timeout_milliseconds: u64,
    cancellation: *const ScalardbAnalyticsConnectorCancellation,
    out_list: *mut *mut ScalardbAnalyticsConnectorNamespaceList,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // The output is checked first, so a null pointer is rejected before
        // the source is queried or a result is built.
        require_output(out_list, "out_list")?;
        // SAFETY: the caller passes valid handles, a filter, or null.
        let (session, namespaces, cancellation) = unsafe {
            (
                &reference(session, "session")?.state,
                read_name_filter(namespaces, "the namespace filter")?,
                optional_cancellation(cancellation),
            )
        };
        let entries =
            session.namespace_list(&namespaces, timeout(timeout_milliseconds), cancellation)?;
        let mut arena = Arena::default();
        let view = arena.namespace_list(&entries);
        // SAFETY: the caller passes a writable output pointer.
        unsafe { write_owned(out_list, view, arena, "out_list") }
    })
}

unsafe extern "C" fn namespace_list_free(list: *mut ScalardbAnalyticsConnectorNamespaceList) {
    // SAFETY: forwarded from the caller.
    unsafe { Owned::free(list) }
}

unsafe extern "C" fn table_list(
    session: *const ScalardbAnalyticsConnectorSession,
    namespaces: *const ScalardbAnalyticsConnectorNameFilter,
    tables: *const ScalardbAnalyticsConnectorNameFilter,
    timeout_milliseconds: u64,
    cancellation: *const ScalardbAnalyticsConnectorCancellation,
    out_list: *mut *mut ScalardbAnalyticsConnectorTableList,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // The output is checked first, so a null pointer is rejected before
        // the source is queried or a result is built.
        require_output(out_list, "out_list")?;
        // SAFETY: the caller passes valid handles, filters, or null.
        let (session, namespaces, tables, cancellation) = unsafe {
            (
                &reference(session, "session")?.state,
                read_name_filter(namespaces, "the namespace filter")?,
                read_name_filter(tables, "the table filter")?,
                optional_cancellation(cancellation),
            )
        };
        let entries = session.table_list(
            &namespaces,
            &tables,
            timeout(timeout_milliseconds),
            cancellation,
        )?;
        let mut arena = Arena::default();
        let view = arena.table_list(&entries);
        // SAFETY: the caller passes a writable output pointer.
        unsafe { write_owned(out_list, view, arena, "out_list") }
    })
}

unsafe extern "C" fn table_list_free(list: *mut ScalardbAnalyticsConnectorTableList) {
    // SAFETY: forwarded from the caller.
    unsafe { Owned::free(list) }
}

unsafe extern "C" fn table_metadata(
    session: *const ScalardbAnalyticsConnectorSession,
    tables: *const ScalardbAnalyticsConnectorNames,
    table_count: usize,
    timeout_milliseconds: u64,
    cancellation: *const ScalardbAnalyticsConnectorCancellation,
    out_result: *mut *mut ScalardbAnalyticsConnectorTableMetadataResult,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // The output is checked first, so a null pointer is rejected before
        // the source is queried or a result is built.
        require_output(out_result, "out_result")?;
        // SAFETY: the caller passes valid handles, name sequences, or null.
        let (session, tables, cancellation) = unsafe {
            (
                &reference(session, "session")?.state,
                read_slice(tables, table_count, "the tables")?
                    .iter()
                    .map(|table| read_table_reference(table, "a requested table"))
                    .collect::<Result<Vec<_>, _>>()?,
                optional_cancellation(cancellation),
            )
        };
        let result =
            session.table_metadata(&tables, timeout(timeout_milliseconds), cancellation)?;
        let mut arena = Arena::default();
        let view = arena.table_metadata_result(&result);
        // SAFETY: the caller passes a writable output pointer.
        unsafe { write_owned(out_result, view, arena, "out_result") }
    })
}

unsafe extern "C" fn table_metadata_result_free(
    result: *mut ScalardbAnalyticsConnectorTableMetadataResult,
) {
    // SAFETY: forwarded from the caller.
    unsafe { Owned::free(result) }
}

unsafe extern "C" fn table_metadata_compare(
    before: *const ScalardbAnalyticsConnectorTableMetadata,
    after: *const ScalardbAnalyticsConnectorTableMetadata,
    out_differences: *mut *mut ScalardbAnalyticsConnectorTableMetadataDifferences,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // The output is checked first, so a null pointer is rejected before
        // the source is queried or a result is built.
        require_output(out_differences, "out_differences")?;
        // SAFETY: the caller passes valid table metadata.
        let (before, after) = unsafe {
            (
                read_table_metadata(reference(before, "before")?)?,
                read_table_metadata(reference(after, "after")?)?,
            )
        };
        let differences = before.compare(&after).map(|differences| {
            let mut arena = Arena::default();
            let view = arena.table_metadata_differences(&differences);
            Owned::into_raw(view, arena)
        });
        // SAFETY: the caller passes a writable output pointer.
        unsafe {
            write(
                out_differences,
                differences.unwrap_or(ptr::null_mut()),
                "out_differences",
            )
        }
    })
}

unsafe extern "C" fn table_metadata_differences_free(
    differences: *mut ScalardbAnalyticsConnectorTableMetadataDifferences,
) {
    // SAFETY: forwarded from the caller.
    unsafe { Owned::free(differences) }
}

unsafe extern "C" fn source_metadata_compare(
    before: *const ScalardbAnalyticsConnectorSourceMetadata,
    after: *const ScalardbAnalyticsConnectorSourceMetadata,
    out_differences: *mut *mut ScalardbAnalyticsConnectorSourceMetadataDifferences,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // The output is checked first, so a null pointer is rejected before
        // the source is queried or a result is built.
        require_output(out_differences, "out_differences")?;
        // SAFETY: the caller passes valid source metadata.
        let (before, after) = unsafe {
            (
                read_source_metadata(reference(before, "before")?)?,
                read_source_metadata(reference(after, "after")?)?,
            )
        };
        let differences = library::current()?
            .compare_source_metadata(&before, &after)?
            .map(|differences| {
                let mut arena = Arena::default();
                let view = arena.source_metadata_differences(&differences);
                Owned::into_raw(view, arena)
            });
        // SAFETY: the caller passes a writable output pointer.
        unsafe {
            write(
                out_differences,
                differences.unwrap_or(ptr::null_mut()),
                "out_differences",
            )
        }
    })
}

unsafe extern "C" fn source_metadata_differences_free(
    differences: *mut ScalardbAnalyticsConnectorSourceMetadataDifferences,
) {
    // SAFETY: forwarded from the caller.
    unsafe { Owned::free(differences) }
}

/// Writes an output tree to a required output pointer, freeing it when the
/// pointer is null.
///
/// # Safety
///
/// A non-null `pointer` is writable.
unsafe fn write_owned<V>(
    pointer: *mut *mut V,
    view: V,
    arena: Arena,
    name: &str,
) -> Result<(), ConnectorError> {
    if pointer.is_null() {
        return Err(invalid_request(&format!("{name} is null")));
    }
    // SAFETY: forwarded from the caller.
    unsafe { pointer.write(Owned::into_raw(view, arena)) };
    Ok(())
}

/// Rejects a null output pointer.
fn require_output<T>(pointer: *mut T, name: &str) -> Result<(), ConnectorError> {
    if pointer.is_null() {
        return Err(invalid_request(&format!("{name} is null")));
    }
    Ok(())
}

/// Borrows an optional cancellation.
///
/// # Safety
///
/// A non-null `cancellation` is a live handle.
unsafe fn optional_cancellation<'a>(
    cancellation: *const ScalardbAnalyticsConnectorCancellation,
) -> Option<&'a Cancellation> {
    // SAFETY: forwarded from the caller.
    unsafe { cancellation.as_ref() }.map(|handle| &handle.cancellation)
}

/// Writes a started operation's handle and its exported result stream.
///
/// # Safety
///
/// Both output pointers are non-null and writable.
unsafe fn write_operation(
    operation: Arc<Operation>,
    out_operation: *mut *mut ScalardbAnalyticsConnectorOperation,
    out_stream: *mut FFI_ArrowArrayStream,
) {
    let stream = stream::export(Arc::clone(&operation));
    let handle = Box::new(ScalardbAnalyticsConnectorOperation {
        operation,
        error: OnceLock::new(),
    });
    // SAFETY: forwarded from the caller.
    unsafe {
        out_stream.write(stream);
        out_operation.write(Box::into_raw(handle));
    }
}

/// Reads a name filter; a null pointer is no filter.
///
/// # Safety
///
/// A non-null `filter` points to a filter whose `names` holds `name_count`
/// NUL-terminated strings.
unsafe fn read_name_filter(
    filter: *const ScalardbAnalyticsConnectorNameFilter,
    name: &str,
) -> Result<NameFilter, ConnectorError> {
    // SAFETY: forwarded from the caller.
    let Some(filter) = (unsafe { filter.as_ref() }) else {
        return Ok(None);
    };
    // SAFETY: forwarded from the caller.
    let names = unsafe { read_slice(filter.names, filter.name_count, name) }?;
    names
        .iter()
        // SAFETY: forwarded from the caller.
        .map(|&text| unsafe { read_text(text, "a name in a filter") })
        .collect::<Result<BTreeSet<_>, _>>()
        .map(Some)
}

unsafe extern "C" fn operation_cancel(
    operation: *const ScalardbAnalyticsConnectorOperation,
) -> *mut ScalardbAnalyticsConnectorError {
    contain(|| {
        // SAFETY: the caller passes an open handle.
        unsafe { reference(operation, "operation") }?
            .operation
            .cancel()
    })
}

unsafe extern "C" fn operation_error(
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

unsafe extern "C" fn operation_close(operation: *mut ScalardbAnalyticsConnectorOperation) {
    // SAFETY: the caller passes an open handle or null.
    unsafe { close(operation, |handle| handle.operation.close()) }
}

unsafe extern "C" fn error_free(error: *mut ScalardbAnalyticsConnectorError) {
    // SAFETY: forwarded from the caller.
    unsafe { OwnedError::free(error) }
}

unsafe extern "C" fn owned_bytes_free(bytes: ScalardbAnalyticsConnectorOwnedBytes) {
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
