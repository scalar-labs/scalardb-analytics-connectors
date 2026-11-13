//! Calls the C ABI the way a C caller does, for the built-in Connectors.

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
    RecordBatch, StructArray,
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
    /// Opens a Data Source of the test Connector.
    pub fn open(options: &[(&str, &str)]) -> Result<Self, Error> {
        Self::open_connector("test", options)
    }

    /// Opens a Data Source of the named Connector.
    pub fn open_connector(connector: &str, options: &[(&str, &str)]) -> Result<Self, Error> {
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
        let connector = text(connector);
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

// A session may be used from any one thread at a time.
unsafe impl Send for Session {}

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

    /// Lists namespaces; `None` lists every namespace.
    pub fn namespace_list(
        &self,
        namespaces: Option<&[&str]>,
        timeout_milliseconds: u64,
        cancellation: Option<&Cancellation>,
    ) -> Result<Owned<ScalardbAnalyticsConnectorNamespaceList>, Error> {
        let namespaces = NameFilter::new(namespaces);
        let mut list = ptr::null_mut();
        // SAFETY: the handles and filter are live and the output is writable.
        check(unsafe {
            scalardb_analytics_connector_namespace_list(
                self.0,
                namespaces.as_ptr(),
                timeout_milliseconds,
                Cancellation::pointer(cancellation),
                &mut list,
            )
        })?;
        Ok(Owned::new(
            list,
            scalardb_analytics_connector_namespace_list_free,
        ))
    }

    /// Lists tables; `None` matches every name.
    pub fn table_list(
        &self,
        namespaces: Option<&[&str]>,
        tables: Option<&[&str]>,
    ) -> Result<Owned<ScalardbAnalyticsConnectorTableList>, Error> {
        let namespaces = NameFilter::new(namespaces);
        let tables = NameFilter::new(tables);
        let mut list = ptr::null_mut();
        // SAFETY: the handle and filters are live and the output is writable.
        check(unsafe {
            scalardb_analytics_connector_table_list(
                self.0,
                namespaces.as_ptr(),
                tables.as_ptr(),
                0,
                ptr::null(),
                &mut list,
            )
        })?;
        Ok(Owned::new(
            list,
            scalardb_analytics_connector_table_list_free,
        ))
    }

    /// Reads the table metadata of tables in `test.public`.
    pub fn table_metadata(
        &self,
        tables: &[&str],
        timeout_milliseconds: u64,
        cancellation: Option<&Cancellation>,
    ) -> Result<Owned<ScalardbAnalyticsConnectorTableMetadataResult>, Error> {
        let paths: Vec<Vec<&str>> = tables
            .iter()
            .map(|table| vec!["test", "public", table])
            .collect();
        let paths: Vec<&[&str]> = paths.iter().map(Vec::as_slice).collect();
        self.table_metadata_of(&paths, timeout_milliseconds, cancellation)
    }

    /// Reads the table metadata of tables named by full paths.
    pub fn table_metadata_of(
        &self,
        tables: &[&[&str]],
        timeout_milliseconds: u64,
        cancellation: Option<&Cancellation>,
    ) -> Result<Owned<ScalardbAnalyticsConnectorTableMetadataResult>, Error> {
        let tables: Vec<Names> = tables.iter().map(|table| Names::new(table)).collect();
        let views: Vec<_> = tables.iter().map(Names::view).collect();
        let mut result = ptr::null_mut();
        // SAFETY: the handles and names are live and the output is writable.
        check(unsafe {
            scalardb_analytics_connector_table_metadata(
                self.0,
                views.as_ptr(),
                views.len(),
                timeout_milliseconds,
                Cancellation::pointer(cancellation),
                &mut result,
            )
        })?;
        Ok(Owned::new(
            result,
            scalardb_analytics_connector_table_metadata_result_free,
        ))
    }

    /// Reads one table of `test.public` that exists and returns its record.
    pub fn read_table(&self, table: &str) -> Owned<ScalardbAnalyticsConnectorTableMetadataResult> {
        let result = self.table_metadata(&[table], 0, None).unwrap();
        assert!(
            !result.records()[0].table_metadata.is_null(),
            "{table} was read"
        );
        result
    }

    pub fn close(self) {
        // SAFETY: the handle is open and not used again.
        unsafe { scalardb_analytics_connector_session_close(self.0) };
    }
}

/// An output tree the library passed to the caller, freed when dropped.
pub struct Owned<T> {
    pointer: *mut T,
    free: unsafe extern "C" fn(*mut T),
}

impl<T> Owned<T> {
    pub fn new(pointer: *mut T, free: unsafe extern "C" fn(*mut T)) -> Self {
        assert!(!pointer.is_null(), "the library wrote an output");
        Self { pointer, free }
    }
}

impl<T> std::ops::Deref for Owned<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the output is live until dropped.
        unsafe { &*self.pointer }
    }
}

impl<T> Drop for Owned<T> {
    fn drop(&mut self) {
        // SAFETY: the output was returned by the library and is freed once.
        unsafe { (self.free)(self.pointer) };
    }
}

impl Owned<ScalardbAnalyticsConnectorTableMetadataResult> {
    pub fn records(&self) -> &[ScalardbAnalyticsConnectorTableRecord] {
        // SAFETY: the library returns `record_count` records.
        unsafe { items(self.records, self.record_count) }
    }

    /// The table metadata of the first record, which was read.
    pub fn table_metadata(&self) -> &ScalardbAnalyticsConnectorTableMetadata {
        // SAFETY: a read record points to its table metadata.
        unsafe { &*self.records()[0].table_metadata }
    }

    /// The source metadata of the first record, which was read.
    pub fn source_metadata(&self) -> &ScalardbAnalyticsConnectorSourceMetadata {
        // SAFETY: a read record points to its source metadata.
        unsafe { &*self.records()[0].source_metadata }
    }
}

/// Borrows `count` values the library returned; null with no values is empty.
///
/// # Safety
///
/// A non-null `data` points to `count` values that outlive the borrow.
pub unsafe fn items<'a, T>(data: *const T, count: usize) -> &'a [T] {
    if data.is_null() {
        assert_eq!(count, 0);
        return &[];
    }
    // SAFETY: forwarded from the caller.
    unsafe { slice::from_raw_parts(data, count) }
}

/// Reads names the library returned.
pub fn read_names(names: &ScalardbAnalyticsConnectorNames) -> Vec<String> {
    // SAFETY: the library returns `name_count` names.
    unsafe { items(names.names, names.name_count) }
        .iter()
        .map(|&name| read_string(name).unwrap())
        .collect()
}

/// Input text as a structure carries it; keep the source alive for the call.
pub fn string(value: &str) -> ScalardbAnalyticsConnectorString {
    ScalardbAnalyticsConnectorString {
        data: value.as_ptr().cast(),
        length: value.len(),
    }
}

/// A name sequence as a C caller builds it.
pub struct Names {
    // The view points into these; moving the vectors keeps their buffers.
    _texts: Vec<String>,
    strings: Vec<ScalardbAnalyticsConnectorString>,
}

impl Names {
    pub fn new(names: &[&str]) -> Self {
        let texts: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        let strings = texts.iter().map(|text| string(text)).collect();
        Self {
            _texts: texts,
            strings,
        }
    }

    pub fn view(&self) -> ScalardbAnalyticsConnectorNames {
        ScalardbAnalyticsConnectorNames {
            names: self.strings.as_ptr(),
            name_count: self.strings.len(),
        }
    }
}

/// A cancellation handle.
pub struct Cancellation(pub *mut ScalardbAnalyticsConnectorCancellation);

// SAFETY: a cancellation may be cancelled from any thread.
unsafe impl Send for Cancellation {}
// SAFETY: as above.
unsafe impl Sync for Cancellation {}

impl Cancellation {
    pub fn new() -> Self {
        let mut cancellation = ptr::null_mut();
        // SAFETY: the output is writable.
        check(unsafe { scalardb_analytics_connector_cancellation_create(&mut cancellation) })
            .unwrap();
        Self(cancellation)
    }

    pub fn cancel(&self) -> Result<(), Error> {
        // SAFETY: the handle is live.
        check(unsafe { scalardb_analytics_connector_cancellation_cancel(self.0) })
    }

    fn pointer(cancellation: Option<&Self>) -> *const ScalardbAnalyticsConnectorCancellation {
        cancellation.map_or(ptr::null(), |cancellation| cancellation.0)
    }
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        // SAFETY: the handle is live and freed once.
        unsafe { scalardb_analytics_connector_cancellation_free(self.0) };
    }
}

/// Compares two table metadata structures as a C caller does.
pub fn compare_table_metadata(
    before: &ScalardbAnalyticsConnectorTableMetadata,
    after: &ScalardbAnalyticsConnectorTableMetadata,
) -> Result<Option<Owned<ScalardbAnalyticsConnectorTableMetadataDifferences>>, Error> {
    let mut differences = ptr::null_mut();
    // SAFETY: both inputs are valid for the call and the output is writable.
    check(unsafe {
        scalardb_analytics_connector_table_metadata_compare(before, after, &mut differences)
    })?;
    Ok((!differences.is_null()).then(|| {
        Owned::new(
            differences,
            scalardb_analytics_connector_table_metadata_differences_free,
        )
    }))
}

/// Compares two source metadata structures as a C caller does.
pub fn compare_source_metadata(
    before: &ScalardbAnalyticsConnectorSourceMetadata,
    after: &ScalardbAnalyticsConnectorSourceMetadata,
) -> Result<Option<Owned<ScalardbAnalyticsConnectorSourceMetadataDifferences>>, Error> {
    let mut differences = ptr::null_mut();
    // SAFETY: both inputs are valid for the call and the output is writable.
    check(unsafe {
        scalardb_analytics_connector_source_metadata_compare(before, after, &mut differences)
    })?;
    Ok((!differences.is_null()).then(|| {
        Owned::new(
            differences,
            scalardb_analytics_connector_source_metadata_differences_free,
        )
    }))
}

/// A name filter as a C caller builds it; `None` is passed as a null pointer.
pub struct NameFilter {
    // The filter points into these; moving the vectors keeps their buffers.
    _texts: Vec<CString>,
    _pointers: Vec<*const std::ffi::c_char>,
    filter: Option<ScalardbAnalyticsConnectorNameFilter>,
}

impl NameFilter {
    pub fn new(names: Option<&[&str]>) -> Self {
        let texts: Vec<CString> = names
            .unwrap_or_default()
            .iter()
            .map(|name| text(name))
            .collect();
        let pointers: Vec<_> = texts.iter().map(|name| name.as_ptr()).collect();
        let filter = names.map(|_| ScalardbAnalyticsConnectorNameFilter {
            names: pointers.as_ptr(),
            name_count: pointers.len(),
        });
        Self {
            _texts: texts,
            _pointers: pointers,
            filter,
        }
    }

    pub fn as_ptr(&self) -> *const ScalardbAnalyticsConnectorNameFilter {
        self.filter.as_ref().map_or(ptr::null(), ptr::from_ref)
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

/// A started operation, a read or a metadata request, and its stream.
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

    /// Pulls the next batch and imports it; `None` at the end of the stream.
    pub fn next_batch(&mut self) -> Option<RecordBatch> {
        let schema = self.schema();
        let mut array = FFI_ArrowArray::empty();
        // SAFETY: the stream is live and the array output is writable.
        let status = unsafe { (self.0.get_next.unwrap())(&mut self.0, &mut array) };
        assert_eq!(status, 0, "the pull succeeds");
        if array.is_released() {
            return None;
        }
        let schema_ffi = FFI_ArrowSchema::try_from(&schema).unwrap();
        // SAFETY: the library exported a valid array of the stream's schema.
        let data = unsafe { arrow_array::ffi::from_ffi(array, &schema_ffi) }.unwrap();
        Some(RecordBatch::from(StructArray::from(data)))
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
