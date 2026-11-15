//! One SQL statement on an ODBC connection, read as Arrow record batches.
//!
//! # Cancellation
//!
//! The cancel and the stream share a [`State`] that names the ODBC statement
//! handle the statement runs on. A cancellation calls `SQLCancel` on that
//! handle, which interrupts a running `SQLExecDirect` or `SQLFetch`, and
//! records the request, because `SQLCancel` has no effect while no ODBC call is
//! in progress, such as between two fetches. The stream checks the record
//! before each step.
//!
//! `SQLCancel` is called only while the statement handle is guaranteed to be
//! allocated: the state leaves [`State::Running`] before any step that may free
//! the handle, and the cancellation holds the lock for the duration of the
//! call.

use std::{
    marker::PhantomData,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    task::{Context, Poll},
};

use arrow_array::RecordBatch;
use arrow_odbc::{
    OdbcReader, OdbcReaderBuilder, arrow_schema_from,
    odbc_api::{
        Connection, ConnectionTransitions, CursorImpl, DataType as OdbcDataType, OwnedCursor,
        Preallocated, ResultSetMetadata, Shared,
        handles::{Statement as _, StatementConnection},
        sys::{HStmt, SQLCancel, SqlReturn},
    },
};
use arrow_schema::{DataType, Schema, SchemaRef};
use connector_sdk::{ConnectorError, ErrorKind, ExecutionCancel, ExecutionOutput};
use futures_core::Stream;

use crate::{
    OdbcConnection, OdbcConnector,
    error::{batch_error, cancelled_error, odbc_error, reader_error},
};

type Cursor = OwnedCursor<Shared<Connection<'static>>>;

/// Starts one statement on `connection`.
///
/// Nothing is sent before the first pull, which executes the statement and
/// reads its first batch. A statement without a result set returns no
/// batches. Each column's Arrow type follows the SQL type the ODBC driver
/// reports, except that `FLOAT` is read as `Float64`
/// ([`result_schema`]). A value that cannot be carried exactly, including one
/// longer than the column size the ODBC driver reports, is a `data` error.
/// Connector `C` classifies every ODBC error the statement reports.
pub fn query<C: OdbcConnector>(connection: &OdbcConnection<C>, sql: String) -> ExecutionOutput {
    let state = Arc::new(Mutex::new(State::Pending));
    let stream = OdbcQueryStream::<C> {
        connection: connection.connection.clone(),
        sql: Some(sql),
        reader: None,
        state: Arc::clone(&state),
        _connector: PhantomData,
    };
    ExecutionOutput::new(Box::pin(stream), Arc::new(OdbcQueryCancel { state }))
}

/// The statement a cancellation reaches.
enum State {
    /// Nothing has been sent to the Data Source.
    Pending,
    /// The statement executes on, or its result is read through, `handle`,
    /// which stays allocated while the statement is in this state.
    Running(StatementHandle),
    /// Between steps whose statement handle may be freed; a cancellation is
    /// only recorded.
    Preparing,
    /// The statement was cancelled; its remaining steps fail.
    Cancelled,
    /// The statement ended or failed; it accepts no further work.
    Finished,
}

/// A raw ODBC statement handle that a cancellation reaches.
#[derive(Clone, Copy)]
struct StatementHandle(HStmt);

// SAFETY: ODBC allows `SQLCancel` on a statement handle from any thread; the
// handle is only used for that while it is allocated (see the module docs).
unsafe impl Send for StatementHandle {}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // Every update is a single assignment, so the state stays consistent
    // across a panic.
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Moves the statement to `next`, unless it was cancelled.
///
/// Returns the cancellation error instead when it was; a cancelled statement
/// stays cancelled.
fn advance(state: &Mutex<State>, next: State) -> Result<(), ConnectorError> {
    let mut current = lock(state);
    if matches!(*current, State::Cancelled) {
        return Err(cancelled_error());
    }
    *current = next;
    Ok(())
}

/// Cancels the statement of one [`query`].
struct OdbcQueryCancel {
    state: Arc<Mutex<State>>,
}

impl ExecutionCancel for OdbcQueryCancel {
    fn cancel(&self) -> Result<(), ConnectorError> {
        let mut state = lock(&self.state);
        match &*state {
            State::Cancelled | State::Finished => return Ok(()),
            State::Running(handle) => {
                // SAFETY: the handle is allocated while the statement is
                // running, and the lock held here keeps it from being freed.
                let result = unsafe { SQLCancel(handle.0) };
                if result != SqlReturn::SUCCESS && result != SqlReturn::SUCCESS_WITH_INFO {
                    return Err(ConnectorError::new(
                        ErrorKind::DataSource,
                        false,
                        "the ODBC driver failed to cancel the statement",
                    ));
                }
            }
            State::Pending | State::Preparing => {}
        }
        *state = State::Cancelled;
        Ok(())
    }
}

/// The result of one statement, read batch by batch.
///
/// Dropping the stream closes the cursor and frees its statement handle,
/// after moving the state out of [`State::Running`].
struct OdbcQueryStream<C> {
    connection: Shared<Connection<'static>>,
    /// The statement, until the first pull executes it.
    sql: Option<String>,
    reader: Option<OdbcReader<Cursor>>,
    state: Arc<Mutex<State>>,
    _connector: PhantomData<fn() -> C>,
}

impl<C> Unpin for OdbcQueryStream<C> {}

impl<C: OdbcConnector> OdbcQueryStream<C> {
    /// Executes the statement and prepares its reader, or returns `None` when
    /// it produced no result set.
    fn start(&mut self, sql: &str) -> Result<Option<OdbcReader<Cursor>>, ConnectorError> {
        advance(&self.state, State::Preparing)?;
        let preallocated = (&self.connection.lock())
            .into_preallocated()
            .map_err(|error| odbc_error::<C>(&error))?;
        let mut statement: StatementConnection<Shared<Connection<'static>>> =
            preallocated.into_handle();
        let handle = StatementHandle(statement.as_stmt_ref().as_sys());
        // SAFETY: `statement` is the freshly allocated handle taken out of a
        // `Preallocated` just above.
        let mut preallocated = unsafe { Preallocated::new(statement) };

        advance(&self.state, State::Running(handle))?;
        // Execute through a borrowed handle so that a failure does not free it
        // while a cancellation may still reach it.
        let executed = preallocated
            .execute(sql, ())
            .map(|cursor| cursor.map(CursorImpl::into_stmt).is_some());
        let cancelled = advance(&self.state, State::Preparing);
        let has_result = match (executed, cancelled) {
            (_, Err(cancelled)) => return Err(cancelled),
            (Err(error), Ok(())) => return Err(odbc_error::<C>(&error)),
            (Ok(has_result), Ok(())) => has_result,
        };
        if !has_result {
            return Ok(None);
        }
        // SAFETY: executing the statement put it in cursor state.
        let mut cursor = unsafe { CursorImpl::new(preallocated.into_handle()) };
        let schema = result_schema::<C>(&mut cursor)?;
        // Building the reader frees the handle if it fails, so the state stays
        // preparing until the reader holds the cursor.
        let reader = OdbcReaderBuilder::new()
            .with_schema(schema)
            .build(cursor)
            .map_err(reader_error::<C>)?;
        advance(&self.state, State::Running(handle))?;
        Ok(Some(reader))
    }

    /// Ends the statement, releasing its reader after the state no longer
    /// names its handle.
    fn finish(&mut self) {
        *lock(&self.state) = State::Finished;
        self.reader = None;
    }

    /// Ends the statement with `error`, reporting a cancellation as such.
    fn fail(&mut self, error: ConnectorError) -> Poll<Option<Result<RecordBatch, ConnectorError>>> {
        let error = if matches!(*lock(&self.state), State::Cancelled) {
            cancelled_error()
        } else {
            error
        };
        self.finish();
        Poll::Ready(Some(Err(error)))
    }
}

impl<C: OdbcConnector> Stream for OdbcQueryStream<C> {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(sql) = this.sql.take() {
            match this.start(&sql) {
                Ok(Some(reader)) => this.reader = Some(reader),
                Ok(None) => {
                    this.finish();
                    return Poll::Ready(None);
                }
                Err(error) => return this.fail(error),
            }
        }
        if matches!(*lock(&this.state), State::Cancelled) {
            return this.fail(cancelled_error());
        }
        let Some(reader) = this.reader.as_mut() else {
            return Poll::Ready(None);
        };
        match reader.next() {
            Some(Ok(batch)) => Poll::Ready(Some(Ok(batch))),
            Some(Err(error)) => this.fail(batch_error::<C>(error)),
            None => {
                this.finish();
                Poll::Ready(None)
            }
        }
    }
}

impl<C> Drop for OdbcQueryStream<C> {
    fn drop(&mut self) {
        *lock(&self.state) = State::Finished;
        self.reader = None;
    }
}

/// The Arrow schema a result set is read as.
///
/// It is the schema `arrow-odbc` derives from the column descriptions, except
/// that a `FLOAT` column is read as `Float64`. ODBC defines `FLOAT(p)` with a
/// binary precision `p` and reads it as a C `double` by default, while some
/// ODBC drivers, such as the PostgreSQL ODBC driver for `double precision`,
/// report a decimal precision that `arrow-odbc` would map to `Float32`,
/// losing digits.
fn result_schema<C: OdbcConnector>(
    cursor: &mut impl ResultSetMetadata,
) -> Result<SchemaRef, ConnectorError> {
    let schema = arrow_schema_from(cursor, None, false).map_err(reader_error::<C>)?;
    let mut fields = Vec::with_capacity(schema.fields().len());
    for (index, field) in (1..).zip(schema.fields().iter()) {
        let field = match cursor
            .col_data_type(index)
            .map_err(|error| odbc_error::<C>(&error))?
        {
            OdbcDataType::Float { .. } => field.as_ref().clone().with_data_type(DataType::Float64),
            _ => field.as_ref().clone(),
        };
        fields.push(field);
    }
    Ok(Arc::new(Schema::new(fields)))
}
