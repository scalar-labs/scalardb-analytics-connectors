//! Metadata requests as the runtime runs them: synchronous calls that return
//! their whole result, and the cancellation a caller passes to them.

use std::{
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};

use futures_core::Stream;

use crate::{
    Component, ConnectorError, ConnectorStream, ErrorKind, ExecutionCancelHandle, TableMetadata,
    TableRecord,
};

use super::lock;

/// What a table metadata request read for one table that exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRead {
    /// The source-independent table metadata the connector derived.
    pub table_metadata: TableMetadata,
    pub source_metadata: SourceMetadata,
}

/// A connector's source metadata for one table, with the connector whose
/// shape it has.
///
/// Only that connector knows the shape, so it is the one that restores the
/// document to compare it ([`super::Library::compare_source_metadata`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMetadata {
    /// The name of the connector that produced the document.
    pub connector: String,
    /// The source metadata as JSON text.
    pub document: String,
}

/// The result of one table metadata request: one record per requested table,
/// in request order, and how they were retrieved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableMetadataResult {
    pub records: Vec<TableRecord<TableRead>>,
    pub retrieval: Retrieval,
}

/// How a table metadata request was retrieved. It is retrieval info, not
/// content: comparisons never use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Retrieval {
    pub connector: Component,
    /// The ADBC driver the session reads through; `None` with direct access.
    pub adbc_driver: Option<Component>,
    pub library_version: String,
}

/// A caller's means of cancelling metadata requests from any thread.
///
/// Cancelling stops every request running with it and makes every later
/// request given it return `cancellation` before anything is sent to the Data
/// Source; a cancellation is not reset.
#[derive(Default)]
pub struct Cancellation {
    state: Mutex<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    cancelled: bool,
    /// The source reads running with this cancellation, by registration id.
    running: Vec<(u64, ExecutionCancelHandle)>,
    next_id: u64,
}

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every request running with this cancellation and every later one.
    ///
    /// Every running request is cancelled even when cancelling one of them
    /// fails; the first failure is returned.
    pub fn cancel(&self) -> Result<(), ConnectorError> {
        let running: Vec<_> = {
            let mut state = lock(&self.state);
            state.cancelled = true;
            state
                .running
                .iter()
                .map(|(_, handle)| handle.clone())
                .collect()
        };
        let results: Vec<_> = running.iter().map(ExecutionCancelHandle::cancel).collect();
        results.into_iter().collect()
    }

    /// Fails with `cancellation` when the caller already cancelled.
    pub(super) fn ensure_not_cancelled(&self) -> Result<(), ConnectorError> {
        if lock(&self.state).cancelled {
            return Err(cancelled());
        }
        Ok(())
    }

    /// Lets a cancellation stop a started source read until the returned
    /// registration is dropped.
    ///
    /// A cancellation that arrived after [`Self::ensure_not_cancelled`] stops
    /// the read at once, so its next pull returns `cancellation`.
    pub(super) fn register(
        &self,
        handle: ExecutionCancelHandle,
    ) -> Result<CancellationRegistration<'_>, ConnectorError> {
        let mut state = lock(&self.state);
        if state.cancelled {
            drop(state);
            handle.cancel()?;
            return Ok(CancellationRegistration {
                cancellation: self,
                id: None,
            });
        }
        let id = state.next_id;
        state.next_id += 1;
        state.running.push((id, handle));
        Ok(CancellationRegistration {
            cancellation: self,
            id: Some(id),
        })
    }
}

/// One source read's place in a [`Cancellation`], removed when dropped.
pub(super) struct CancellationRegistration<'a> {
    cancellation: &'a Cancellation,
    id: Option<u64>,
}

impl Drop for CancellationRegistration<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            lock(&self.cancellation.state)
                .running
                .retain(|(running, _)| *running != id);
        }
    }
}

fn cancelled() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Cancellation,
        false,
        "the metadata request was cancelled",
    )
}

/// Converts each item of a connector stream, ending at the first error.
pub(super) struct MapItems<T, U> {
    items: ConnectorStream<T>,
    convert: Box<dyn Fn(T) -> Result<U, ConnectorError> + Send>,
}

impl<T, U> MapItems<T, U> {
    pub(super) fn new(
        items: ConnectorStream<T>,
        convert: impl Fn(T) -> Result<U, ConnectorError> + Send + 'static,
    ) -> Self {
        Self {
            items,
            convert: Box::new(convert),
        }
    }
}

impl<T, U> Stream for MapItems<T, U> {
    type Item = Result<U, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.items
            .as_mut()
            .poll_next(context)
            .map(|item| item.map(|item| item.and_then(&this.convert)))
    }
}
