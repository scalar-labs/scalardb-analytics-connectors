//! A read operation's results as an Arrow C stream.

use std::{
    io,
    panic::{self, AssertUnwindSafe},
    sync::Arc,
};

use arrow_array::{RecordBatch, RecordBatchReader, ffi_stream::FFI_ArrowArrayStream};
use arrow_schema::{ArrowError, SchemaRef};

use connector_sdk::runtime::ReadOperation;

/// Exports the operation's results as an `ArrowArrayStream`.
///
/// The stream holds no Data Source resources of its own: it pulls through the
/// operation, and releasing it releases the operation's read.
pub(super) fn export(operation: Arc<ReadOperation>) -> FFI_ArrowArrayStream {
    let schema = operation.output_schema();
    FFI_ArrowArrayStream::new(Box::new(OperationReader {
        operation: Some(operation),
        schema,
    }))
}

struct OperationReader {
    /// Present until the stream is released.
    operation: Option<Arc<ReadOperation>>,
    schema: SchemaRef,
}

impl Iterator for OperationReader {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        let operation = self.operation.as_ref()?;
        match operation.pull() {
            Ok(batch) => batch.map(Ok),
            // The Arrow C stream carries only an error number and a string; the
            // caller retrieves the structured error from the operation. The
            // string is the safe message and must not contain NUL.
            Err(error) => Some(Err(ArrowError::IoError(
                error.to_string().replace('\0', " "),
                io::Error::other("connector error"),
            ))),
        }
    }
}

impl RecordBatchReader for OperationReader {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }
}

impl Drop for OperationReader {
    /// Runs inside the Arrow `release` callback, which cannot unwind.
    ///
    /// When the caller closed the operation and its ancestors first, this is
    /// the last reference to them, so dropping it releases the connector's
    /// session and Data Source; a panic there is contained with the release.
    fn drop(&mut self) {
        if let Some(operation) = self.operation.take() {
            // Nothing remains to report a panic to once the stream is released.
            let _ = panic::catch_unwind(AssertUnwindSafe(move || {
                operation.release();
                drop(operation);
            }));
        }
    }
}
