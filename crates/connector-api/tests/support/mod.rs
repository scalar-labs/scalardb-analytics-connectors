//! Calls the C ABI the way a C caller does, for the test Connector.

#![allow(dead_code)]

use std::{
    ffi::{CStr, CString},
    ptr, slice,
    sync::{
        Arc, Once,
        atomic::{AtomicUsize, Ordering},
    },
};

use arrow_array::{
    ffi::{FFI_ArrowArray, FFI_ArrowSchema},
    ffi_stream::FFI_ArrowArrayStream,
};
use arrow_schema::Schema;
use connector_api::abi::*;
use connector_sdk::test_connector;
use prost::Message;
use substrait::proto::{
    NamedStruct, Plan, PlanRel, ReadRel, Rel, RelRoot, Type, plan_rel, read_rel, rel, r#type,
};

/// Input text as a C caller passes it; keep the value alive for the call.
pub fn text(value: &str) -> CString {
    CString::new(value).unwrap()
}

pub fn bytes(value: &[u8]) -> ScalardbAnalyticsConnectorBytes {
    ScalardbAnalyticsConnectorBytes {
        data: value.as_ptr(),
        length: value.len(),
    }
}

/// Reads text the library returned; `None` when absent.
///
/// It also checks the promised NUL terminator after `length` bytes.
pub fn read_string(value: ScalardbAnalyticsConnectorString) -> Option<String> {
    if value.data.is_null() {
        return None;
    }
    // SAFETY: the library returns `length` readable UTF-8 bytes followed by NUL.
    let bytes = unsafe { slice::from_raw_parts(value.data.cast::<u8>(), value.length + 1) };
    assert_eq!(bytes[value.length], 0, "output text is NUL-terminated");
    Some(String::from_utf8(bytes[..value.length].to_vec()).unwrap())
}

/// An error the library returned, copied and freed.
#[derive(Debug)]
pub struct Error {
    pub kind: ScalardbAnalyticsConnectorErrorKind,
    pub retryable: bool,
    pub message: String,
}

impl Error {
    /// Copies the error's common fields.
    ///
    /// # Safety
    ///
    /// `error` points to a live error.
    pub unsafe fn copy(error: *const ScalardbAnalyticsConnectorError) -> Self {
        // SAFETY: forwarded from the caller.
        let error = unsafe { &*error };
        Self {
            kind: error.kind,
            retryable: error.retryable,
            message: read_string(error.message).unwrap(),
        }
    }
}

/// Converts a function's returned error into a `Result`, freeing it.
pub fn check(error: *mut ScalardbAnalyticsConnectorError) -> Result<(), Error> {
    if error.is_null() {
        return Ok(());
    }
    // SAFETY: a non-null error returned by the library is live until freed.
    let copied = unsafe { Error::copy(error) };
    // SAFETY: the error was returned by the library and is freed once.
    unsafe { scalardb_analytics_connector_error_free(error) };
    Err(copied)
}

/// Initializes the library once for every test in this process.
pub fn initialize() {
    static INITIALIZED: Once = Once::new();
    INITIALIZED.call_once(|| {
        let settings = ScalardbAnalyticsConnectorLibrarySettings {
            adbc_driver_directory: ptr::null(),
        };
        // SAFETY: the settings are valid for the call.
        check(unsafe { scalardb_analytics_connector_library_initialize(&settings) }).unwrap();
    });
}

/// Returns a label no other test uses, naming the probe of its Data Sources.
pub fn unique_label() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!("test-{}", NEXT.fetch_add(1, Ordering::SeqCst))
}

pub struct DataSource(pub *mut ScalardbAnalyticsConnectorDataSource);

impl DataSource {
    pub fn open(options: &[(&str, &str)]) -> Result<Self, Error> {
        let texts: Vec<_> = options
            .iter()
            .map(|(key, value)| (text(key), text(value)))
            .collect();
        let options: Vec<_> = texts
            .iter()
            .map(|(key, value)| ScalardbAnalyticsConnectorOption {
                key: key.as_ptr(),
                value: value.as_ptr(),
            })
            .collect();
        let connector = text("test");
        let mut data_source = ptr::null_mut();
        // SAFETY: every input lives for the call and the output is writable.
        check(unsafe {
            scalardb_analytics_connector_data_source_open(
                connector.as_ptr(),
                options.as_ptr(),
                options.len(),
                0,
                &mut data_source,
            )
        })?;
        Ok(Self(data_source))
    }

    /// Opens a test Data Source whose probe is returned with it.
    pub fn open_probed() -> (Self, Arc<test_connector::Probe>) {
        let label = unique_label();
        let data_source = Self::open(&[("label", &label)]).unwrap();
        (data_source, test_connector::probe(&label))
    }

    pub fn session(&self) -> Result<Session, Error> {
        let mut session = ptr::null_mut();
        // SAFETY: the handle is open and the output is writable.
        check(unsafe { scalardb_analytics_connector_session_open(self.0, &mut session) })?;
        Ok(Session(session))
    }

    pub fn close(self) {
        // SAFETY: the handle is open and not used again.
        unsafe { scalardb_analytics_connector_data_source_close(self.0) };
    }
}

pub struct Session(pub *mut ScalardbAnalyticsConnectorSession);

impl Session {
    pub fn plan(&self, table: &[&str]) -> Result<PushdownResult, Error> {
        self.plan_bytes(&read_plan(table))
    }

    pub fn plan_bytes(&self, offered_plan: &[u8]) -> Result<PushdownResult, Error> {
        let mut result = ptr::null_mut();
        // SAFETY: the handle is open, the bytes live for the call, and the
        // output is writable.
        check(unsafe {
            scalardb_analytics_connector_plan_pushdown(self.0, bytes(offered_plan), &mut result)
        })?;
        Ok(PushdownResult(result))
    }

    /// Plans a read of `table` and returns its remote execution.
    pub fn remote(&self, table: &[&str]) -> RemoteExecution {
        self.plan(table).unwrap().remote_execution().unwrap()
    }

    pub fn restore(&self, encoded: &[u8]) -> Result<RemoteExecution, Error> {
        let mut remote = ptr::null_mut();
        // SAFETY: the handle is open, the bytes live for the call, and the
        // output is writable.
        check(unsafe {
            scalardb_analytics_connector_remote_execution_restore(
                self.0,
                bytes(encoded),
                &mut remote,
            )
        })?;
        Ok(RemoteExecution(remote))
    }

    pub fn read(&self, remote: &RemoteExecution, timeout_milliseconds: u64) -> Result<Read, Error> {
        let mut operation = ptr::null_mut();
        let mut stream = FFI_ArrowArrayStream::empty();
        // SAFETY: the handles are live and the outputs are writable.
        check(unsafe {
            scalardb_analytics_connector_read_start(
                self.0,
                remote.0,
                timeout_milliseconds,
                &mut operation,
                &mut stream,
            )
        })?;
        Ok(Read {
            operation: Operation(operation),
            stream: Stream(stream),
        })
    }

    pub fn close(self) {
        // SAFETY: the handle is open and not used again.
        unsafe { scalardb_analytics_connector_session_close(self.0) };
    }
}

pub struct PushdownResult(pub *mut ScalardbAnalyticsConnectorPushdownResult);

impl PushdownResult {
    pub fn remote_execution(&self) -> Result<RemoteExecution, Error> {
        let mut remote = ptr::null_mut();
        // SAFETY: the result is live and the output is writable.
        check(unsafe {
            scalardb_analytics_connector_pushdown_result_remote_execution(self.0, &mut remote)
        })?;
        Ok(RemoteExecution(remote))
    }
}

impl Drop for PushdownResult {
    fn drop(&mut self) {
        // SAFETY: the result is live and freed once.
        unsafe { scalardb_analytics_connector_pushdown_result_free(self.0) };
    }
}

pub struct RemoteExecution(pub *mut ScalardbAnalyticsConnectorRemoteExecution);

impl RemoteExecution {
    pub fn encode(&self) -> Vec<u8> {
        let mut encoded = ScalardbAnalyticsConnectorOwnedBytes {
            data: ptr::null_mut(),
            length: 0,
        };
        // SAFETY: the handle is live and the output is writable.
        check(unsafe {
            scalardb_analytics_connector_remote_execution_encode(self.0, &mut encoded)
        })
        .unwrap();
        // SAFETY: the library returned readable bytes of this length.
        let copied = unsafe { slice::from_raw_parts(encoded.data, encoded.length) }.to_vec();
        // SAFETY: the bytes were returned by the library and are freed once.
        unsafe { scalardb_analytics_connector_owned_bytes_free(encoded) };
        copied
    }
}

impl Drop for RemoteExecution {
    fn drop(&mut self) {
        // SAFETY: the handle is live and freed once.
        unsafe { scalardb_analytics_connector_remote_execution_free(self.0) };
    }
}

pub struct Read {
    pub operation: Operation,
    pub stream: Stream,
}

pub struct Operation(pub *mut ScalardbAnalyticsConnectorOperation);

// The operation handle may be cancelled from any thread.
unsafe impl Send for Operation {}
unsafe impl Sync for Operation {}

impl Operation {
    pub fn cancel(&self) -> Result<(), Error> {
        // SAFETY: the handle is open.
        check(unsafe { scalardb_analytics_connector_operation_cancel(self.0) })
    }

    /// Returns the operation's error, which the operation keeps owning.
    pub fn error(&self) -> Option<&ScalardbAnalyticsConnectorError> {
        // SAFETY: the handle is open; the error lives until it is closed,
        // which `self` outlives.
        unsafe { scalardb_analytics_connector_operation_error(self.0).as_ref() }
    }

    pub fn close(self) {
        // SAFETY: the handle is open and not used again.
        unsafe { scalardb_analytics_connector_operation_close(self.0) };
    }
}

/// The caller's `ArrowArrayStream`, driven through its C callbacks.
pub struct Stream(pub FFI_ArrowArrayStream);

// A stream is pulled from one thread at a time, which may differ from the
// thread that started it.
unsafe impl Send for Stream {}

/// The outcome of one `get_next` call.
#[derive(Debug, PartialEq, Eq)]
pub enum Pull {
    /// A batch with this many rows.
    Batch(i64),
    End,
    /// The error number and the stream's last error string.
    Failed(i32, String),
}

impl Stream {
    pub fn schema(&mut self) -> Schema {
        let mut schema = FFI_ArrowSchema::empty();
        // SAFETY: the stream is live and the schema output is writable.
        let status = unsafe { (self.0.get_schema.unwrap())(&mut self.0, &mut schema) };
        assert_eq!(status, 0);
        Schema::try_from(&schema).unwrap()
    }

    pub fn pull(&mut self) -> Pull {
        let mut array = FFI_ArrowArray::empty();
        // SAFETY: the stream is live and the array output is writable.
        let status = unsafe { (self.0.get_next.unwrap())(&mut self.0, &mut array) };
        if status != 0 {
            // SAFETY: after a failed pull the stream returns a NUL-terminated
            // string owned by the stream.
            let message = unsafe { CStr::from_ptr((self.0.get_last_error.unwrap())(&mut self.0)) };
            return Pull::Failed(status, message.to_str().unwrap().to_owned());
        }
        if array.is_released() {
            Pull::End
        } else {
            Pull::Batch(array.len() as i64)
        }
    }

    /// Releases the stream through its `release` callback.
    pub fn release(mut self) {
        // SAFETY: the stream is live and released once; the callback clears it.
        unsafe { (self.0.release.unwrap())(&mut self.0) };
    }
}

/// Encodes a Substrait plan that reads one `bigint` column `value` from `table`.
pub fn read_plan(table: &[&str]) -> Vec<u8> {
    let read = ReadRel {
        base_schema: Some(NamedStruct {
            names: vec!["value".to_owned()],
            r#struct: Some(r#type::Struct {
                types: vec![Type {
                    kind: Some(r#type::Kind::I64(r#type::I64 {
                        nullability: r#type::Nullability::Required as i32,
                        ..Default::default()
                    })),
                }],
                nullability: r#type::Nullability::Required as i32,
                ..Default::default()
            }),
        }),
        read_type: Some(read_rel::ReadType::NamedTable(read_rel::NamedTable {
            names: table.iter().map(|name| (*name).to_owned()).collect(),
            advanced_extension: None,
        })),
        ..Default::default()
    };
    Plan {
        relations: vec![PlanRel {
            rel_type: Some(plan_rel::RelType::Root(RelRoot {
                input: Some(Rel {
                    rel_type: Some(rel::RelType::Read(Box::new(read))),
                }),
                names: vec!["value".to_owned()],
            })),
        }],
        ..Default::default()
    }
    .encode_to_vec()
}
