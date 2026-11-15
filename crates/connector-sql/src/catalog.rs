//! Running a Connector's catalog reads and reading their result rows.
//!
//! A Connector writes each catalog read as a function that runs its
//! statements in order on [`CatalogReads`] and uses each result where it is
//! fetched. [`catalog_read`] turns that function into one stoppable source
//! read.

use std::{
    collections::VecDeque,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};

use arrow_array::{
    Array, BooleanArray, Int16Array, Int32Array, Int64Array, ListArray, RecordBatch, StringArray,
};
use connector_sdk::{ConnectorError, ErrorKind, ExecutionOutput};
use futures_core::Stream;

use crate::{
    SqlConnector,
    step::{Step, StepCancel},
};

/// Runs catalog statements for one catalog read, one at a time.
///
/// Each statement runs to its end before [`CatalogReads::query`] returns. A
/// cancel from another thread stops the running statement, and a cancelled
/// read starts no further statement.
pub struct CatalogReads<'a, C: SqlConnector> {
    connection: &'a C::Connection,
    cancel: &'a StepCancel,
}

impl<C: SqlConnector> CatalogReads<'_, C> {
    /// Runs one statement and returns its result batches.
    ///
    /// Catalog statements read no rows of user tables, so one catalog read
    /// may run several of them.
    pub fn query(&mut self, sql: impl Into<String>) -> Result<Vec<RecordBatch>, ConnectorError> {
        let (mut statement, cancel) = C::query(self.connection, sql.into()).into_parts();
        self.cancel.start(Step::Query(cancel))?;
        let mut context = Context::from_waker(Waker::noop());
        let mut batches = Vec::new();
        loop {
            match statement.as_mut().poll_next(&mut context) {
                Poll::Ready(Some(batch)) => batches.push(batch?),
                Poll::Ready(None) => return Ok(batches),
                Poll::Pending => {
                    return Err(ConnectorError::new(
                        ErrorKind::Internal,
                        false,
                        "a synchronous catalog statement returned pending",
                    ));
                }
            }
        }
    }
}

/// A catalog read: runs its statements on [`CatalogReads`] and returns the
/// read's items.
pub type CatalogFunction<C, T> =
    Box<dyn for<'a> FnOnce(&mut CatalogReads<'a, C>) -> Result<Vec<T>, ConnectorError> + Send>;

/// Runs a catalog read as one source read.
///
/// Nothing is sent before the first pull, which runs `read` to its end and
/// then returns its items one per pull. The returned cancel stops the
/// statement in progress and keeps later statements from starting.
pub fn catalog_read<C: SqlConnector, T: Send + 'static>(
    connection: &C::Connection,
    read: CatalogFunction<C, T>,
) -> ExecutionOutput<T> {
    let cancel = Arc::new(StepCancel::new());
    let stream = CatalogRead {
        connection: connection.clone(),
        read: Some(read),
        items: VecDeque::new(),
        cancel: Arc::clone(&cancel),
    };
    ExecutionOutput::new(Box::pin(stream), cancel)
}

/// Runs a catalog read on the calling thread, for reads no caller cancels,
/// such as a session's context or planning facts.
pub fn read_catalog<C: SqlConnector, T>(
    connection: &C::Connection,
    read: impl FnOnce(&mut CatalogReads<'_, C>) -> Result<T, ConnectorError>,
) -> Result<T, ConnectorError> {
    let cancel = StepCancel::new();
    read(&mut CatalogReads {
        connection,
        cancel: &cancel,
    })
}

struct CatalogRead<C: SqlConnector, T> {
    connection: C::Connection,
    read: Option<CatalogFunction<C, T>>,
    items: VecDeque<T>,
    cancel: Arc<StepCancel>,
}

impl<C: SqlConnector, T> Unpin for CatalogRead<C, T> {}

impl<C: SqlConnector, T> Stream for CatalogRead<C, T> {
    type Item = Result<T, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(read) = this.read.take() {
            let mut reads = CatalogReads {
                connection: &this.connection,
                cancel: &this.cancel,
            };
            match read(&mut reads) {
                Ok(items) => this.items = items.into(),
                Err(error) => return Poll::Ready(Some(Err(error))),
            }
        }
        Poll::Ready(this.items.pop_front().map(Ok))
    }
}

/// Returns a catalog result column as the array type its statement selects.
pub fn column<T: 'static>(batch: &RecordBatch, index: usize) -> Result<&T, ConnectorError> {
    batch
        .columns()
        .get(index)
        .and_then(|column| column.as_any().downcast_ref::<T>())
        .ok_or_else(unexpected_catalog_result)
}

/// The values of a text column, with NULL as `None`.
pub fn text(batch: &RecordBatch, index: usize) -> Result<Vec<Option<&str>>, ConnectorError> {
    Ok(column::<StringArray>(batch, index)?.iter().collect())
}

pub fn int16(batch: &RecordBatch, index: usize) -> Result<&Int16Array, ConnectorError> {
    column(batch, index)
}

pub fn int32(batch: &RecordBatch, index: usize) -> Result<&Int32Array, ConnectorError> {
    column(batch, index)
}

pub fn int64(batch: &RecordBatch, index: usize) -> Result<&Int64Array, ConnectorError> {
    column(batch, index)
}

pub fn boolean(batch: &RecordBatch, index: usize) -> Result<&BooleanArray, ConnectorError> {
    column(batch, index)
}

/// The values of a 16-bit integer list column, with NULL as `None`.
pub fn int16_lists(
    batch: &RecordBatch,
    index: usize,
) -> Result<Vec<Option<Vec<i16>>>, ConnectorError> {
    let lists = column::<ListArray>(batch, index)?;
    (0..lists.len())
        .map(|row| {
            if lists.is_null(row) {
                return Ok(None);
            }
            let values = lists.value(row);
            let values = values
                .as_any()
                .downcast_ref::<Int16Array>()
                .ok_or_else(unexpected_catalog_result)?;
            Ok(Some(values.values().to_vec()))
        })
        .collect()
}

/// A text value the catalog never leaves NULL.
pub fn required(value: Option<&str>) -> Result<&str, ConnectorError> {
    value.ok_or_else(unexpected_catalog_result)
}

/// The error for catalog rows that do not have the shape their statement selects.
pub fn unexpected_catalog_result() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Internal,
        false,
        "a catalog query returned an unexpected result",
    )
}
