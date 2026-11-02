//! Creates one lazy ADBC query execution and exposes its Arrow result as a stream.

use std::{
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    task::{Context, Poll},
};

use adbc_core::{
    CancelHandle, Connection, Statement,
    error::{Error, Status},
};
use adbc_driver_manager::{ManagedConnection, ManagedStatement};
use arrow_array::{RecordBatch, RecordBatchReader};
use futures_core::Stream;

/// Creates a query stream without sending anything to the source.
///
/// The first read prepares an ADBC statement and reader before reading the first
/// batch. Later reads use the same reader and statement.
pub fn query(
    connection: ManagedConnection,
    sql: impl Into<String>,
    parameters: Option<RecordBatch>,
) -> AdbcQueryStream {
    AdbcQueryStream {
        state: Arc::new(Mutex::new(QueryState::Pending(QueryRequest {
            connection,
            sql: sql.into(),
            parameters,
        }))),
    }
}

/// A state machine that owns one lazy, pull-based ADBC query execution.
///
/// Stream reads drive the query through `QueryState`. A cancellation handle can
/// send a `Cancel` event to the same state machine from another thread. The
/// stream owns one strong reference to the shared state, and every cancellation
/// handle owns another. The mutex protects lifecycle transitions, not resources
/// transferred to the polling thread for a blocking source call.
#[must_use = "query execution starts only when the stream is pulled"]
pub struct AdbcQueryStream {
    state: Arc<Mutex<QueryState>>,
}

/// Encodes the current lifecycle phase of one query.
///
/// `Pending` and `Ready` own resources between reads. `Preparing` and `Reading`
/// record a blocking operation while `poll_next` temporarily owns its resources.
enum QueryState {
    /// The query has been accepted but has not contacted the source.
    Pending(QueryRequest),
    /// The statement or result reader required by the query is being prepared.
    Preparing(QueryPreparation),
    /// The query owns a result reader and is ready to read another batch.
    Ready(QueryExecution),
    /// The polling thread owns the execution while reading one batch.
    Reading(Arc<dyn CancelHandle>),
    /// Cancellation has been accepted and no later read may return a batch.
    Cancelled,
    /// The query owns no resources and accepts no further work.
    Finished,
}

/// Identifies which required query resource is currently being created.
enum QueryPreparation {
    /// The polling thread owns the request while `new_statement` is in progress.
    CreatingStatement,
    /// The polling thread owns the statement while creating the result reader.
    CreatingReader(Arc<dyn CancelHandle>),
}

struct QueryRequest {
    connection: ManagedConnection,
    sql: String,
    parameters: Option<RecordBatch>,
}

/// Owns the statement and query arguments used to create the result reader.
struct ReaderCreation {
    sql: String,
    parameters: Option<RecordBatch>,
    statement: StatementResources,
}

/// Owns an ADBC statement together with its statement-scoped cancellation capability.
struct StatementResources {
    cancel_handle: Arc<dyn CancelHandle>,
    statement: ManagedStatement,
}

/// Owns every source resource required to read the remaining query result.
struct QueryExecution {
    // Rust drops fields in declaration order, so the Arrow reader releases its
    // driver callbacks before the statement and its parent handles are released.
    reader: Box<dyn RecordBatchReader + Send + 'static>,
    statement: StatementResources,
}

/// Owns the resource transferred to the polling thread for one blocking read.
enum ReadAction {
    CreateStatement(QueryRequest),
    ReadBatch(QueryExecution),
    ReturnCancellation,
    ReturnEnd,
}

/// A thread-safe capability for sending a `Cancel` event to an `AdbcQueryStream`.
///
/// Each clone retains the shared query state independently of the stream. It
/// locks that state to inspect the current phase and record the cancellation
/// transition, but it never owns the request, statement, or result reader.
#[derive(Clone)]
pub struct AdbcQueryCancelHandle {
    state: Arc<Mutex<QueryState>>,
}

impl AdbcQueryStream {
    /// Returns a cloneable handle for cancelling this query from another thread.
    pub fn cancel_handle(&self) -> AdbcQueryCancelHandle {
        AdbcQueryCancelHandle {
            state: Arc::clone(&self.state),
        }
    }

    fn start_read(&mut self) -> Result<ReadAction, Error> {
        let mut state = lock_state(&self.state)?;
        let current = std::mem::replace(&mut *state, QueryState::Finished);
        match current {
            QueryState::Pending(request) => {
                *state = QueryState::Preparing(QueryPreparation::CreatingStatement);
                Ok(ReadAction::CreateStatement(request))
            }
            QueryState::Ready(execution) => {
                *state = QueryState::Reading(Arc::clone(&execution.statement.cancel_handle));
                Ok(ReadAction::ReadBatch(execution))
            }
            QueryState::Cancelled => Ok(ReadAction::ReturnCancellation),
            QueryState::Finished => Ok(ReadAction::ReturnEnd),
            current @ (QueryState::Preparing(_) | QueryState::Reading(_)) => {
                *state = current;
                Err(invalid_state_error(
                    "the query cannot start another read while source work is in progress",
                ))
            }
        }
    }

    fn finish(&mut self) {
        let retired = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            std::mem::replace(&mut *state, QueryState::Finished)
        };
        drop(retired);
    }
}

impl AdbcQueryCancelHandle {
    /// Requests cancellation of the associated ADBC query execution.
    ///
    /// Cancellation is recorded without a driver callback when no source call is
    /// in progress. While a statement or reader call is in progress, this method
    /// invokes the statement-scoped ADBC cancel handle.
    pub fn try_cancel(&self) -> Result<(), Error> {
        let mut state = lock_state(&self.state)?;
        match &*state {
            QueryState::Pending(_)
            | QueryState::Preparing(QueryPreparation::CreatingStatement)
            | QueryState::Ready(_) => {
                let retired = std::mem::replace(&mut *state, QueryState::Cancelled);
                drop(state);
                drop(retired);
                Ok(())
            }
            QueryState::Preparing(QueryPreparation::CreatingReader(cancel_handle))
            | QueryState::Reading(cancel_handle) => {
                // Keep the state locked so stream cleanup cannot release the
                // statement while its native cancellation callback is running.
                cancel_handle.try_cancel()?;
                let retired = std::mem::replace(&mut *state, QueryState::Cancelled);
                drop(state);
                drop(retired);
                Ok(())
            }
            QueryState::Cancelled | QueryState::Finished => Ok(()),
        }
    }
}

impl CancelHandle for AdbcQueryCancelHandle {
    fn try_cancel(&self) -> Result<(), Error> {
        AdbcQueryCancelHandle::try_cancel(self)
    }
}

impl Stream for AdbcQueryStream {
    type Item = Result<RecordBatch, Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.start_read() {
            Ok(ReadAction::CreateStatement(request)) => {
                prepare_statement_and_read(&this.state, request)
            }
            Ok(ReadAction::ReadBatch(execution)) => read_batch(&this.state, execution),
            Ok(ReadAction::ReturnCancellation) => {
                this.finish();
                Poll::Ready(Some(Err(cancelled_error())))
            }
            Ok(ReadAction::ReturnEnd) => Poll::Ready(None),
            Err(error) => {
                this.finish();
                Poll::Ready(Some(Err(error)))
            }
        }
    }
}

impl Drop for AdbcQueryStream {
    fn drop(&mut self) {
        self.finish();
    }
}

fn prepare_statement_and_read(
    state: &Mutex<QueryState>,
    mut request: QueryRequest,
) -> Poll<Option<Result<RecordBatch, Error>>> {
    let statement = match request.connection.new_statement() {
        Ok(statement) => statement,
        Err(error) => return finish_operation(state, is_creating_statement, error),
    };
    let cancel_handle: Arc<dyn CancelHandle> = statement.get_cancel_handle().into();

    let mut query_state = match lock_state(state) {
        Ok(query_state) => query_state,
        Err(error) => return Poll::Ready(Some(Err(error))),
    };
    match &*query_state {
        QueryState::Preparing(QueryPreparation::CreatingStatement) => {
            *query_state =
                QueryState::Preparing(QueryPreparation::CreatingReader(Arc::clone(&cancel_handle)));
            drop(query_state);
            prepare_reader_and_read(
                state,
                ReaderCreation {
                    sql: request.sql,
                    parameters: request.parameters,
                    statement: StatementResources {
                        cancel_handle,
                        statement,
                    },
                },
            )
        }
        QueryState::Cancelled => {
            *query_state = QueryState::Finished;
            Poll::Ready(Some(Err(cancelled_error())))
        }
        _ => Poll::Ready(Some(Err(invalid_state_error(
            "statement creation completed after its query state changed",
        )))),
    }
}

fn prepare_reader_and_read(
    state: &Mutex<QueryState>,
    mut creation: ReaderCreation,
) -> Poll<Option<Result<RecordBatch, Error>>> {
    if let Err(error) = check_reader_creation(state) {
        return Poll::Ready(Some(Err(error)));
    }
    if let Err(error) = creation.statement.statement.set_sql_query(&creation.sql) {
        return finish_operation(state, is_creating_reader, error);
    }
    if let Err(error) = check_reader_creation(state) {
        return Poll::Ready(Some(Err(error)));
    }

    if let Some(parameters) = creation.parameters.take() {
        if let Err(error) = creation.statement.statement.bind(parameters) {
            return finish_operation(state, is_creating_reader, error);
        }
        if let Err(error) = check_reader_creation(state) {
            return Poll::Ready(Some(Err(error)));
        }
    }

    let reader = match creation.statement.statement.execute() {
        Ok(reader) => reader,
        Err(error) => return finish_operation(state, is_creating_reader, error),
    };
    let execution = QueryExecution {
        reader,
        statement: creation.statement,
    };

    let mut query_state = match lock_state(state) {
        Ok(query_state) => query_state,
        Err(error) => return Poll::Ready(Some(Err(error))),
    };
    match &*query_state {
        QueryState::Preparing(QueryPreparation::CreatingReader(_)) => {
            *query_state = QueryState::Reading(Arc::clone(&execution.statement.cancel_handle));
            drop(query_state);
            read_batch(state, execution)
        }
        QueryState::Cancelled => {
            *query_state = QueryState::Finished;
            Poll::Ready(Some(Err(cancelled_error())))
        }
        _ => Poll::Ready(Some(Err(invalid_state_error(
            "reader creation completed after its query state changed",
        )))),
    }
}

fn read_batch(
    state: &Mutex<QueryState>,
    mut execution: QueryExecution,
) -> Poll<Option<Result<RecordBatch, Error>>> {
    if let Err(error) = check_reading(state) {
        return Poll::Ready(Some(Err(error)));
    }
    let item = execution.reader.next();

    let mut query_state = match lock_state(state) {
        Ok(query_state) => query_state,
        Err(error) => return Poll::Ready(Some(Err(error))),
    };
    match &*query_state {
        QueryState::Reading(_) => match item {
            Some(Ok(batch)) => {
                *query_state = QueryState::Ready(execution);
                Poll::Ready(Some(Ok(batch)))
            }
            Some(Err(error)) => {
                *query_state = QueryState::Finished;
                Poll::Ready(Some(Err(error.into())))
            }
            None => {
                *query_state = QueryState::Finished;
                Poll::Ready(None)
            }
        },
        QueryState::Cancelled => {
            *query_state = QueryState::Finished;
            Poll::Ready(Some(Err(cancelled_error())))
        }
        _ => Poll::Ready(Some(Err(invalid_state_error(
            "batch reading completed after its query state changed",
        )))),
    }
}

fn check_reader_creation(state: &Mutex<QueryState>) -> Result<(), Error> {
    check_operation(state, is_creating_reader, "reader creation")
}

fn check_reading(state: &Mutex<QueryState>) -> Result<(), Error> {
    check_operation(state, is_reading, "batch reading")
}

fn check_operation(
    state: &Mutex<QueryState>,
    is_active: impl FnOnce(&QueryState) -> bool,
    operation: &str,
) -> Result<(), Error> {
    let mut query_state = lock_state(state)?;
    if is_active(&query_state) {
        Ok(())
    } else if matches!(*query_state, QueryState::Cancelled) {
        *query_state = QueryState::Finished;
        Err(cancelled_error())
    } else {
        Err(invalid_state_error(format!(
            "{operation} continued after its query state changed"
        )))
    }
}

fn is_creating_statement(state: &QueryState) -> bool {
    matches!(
        state,
        QueryState::Preparing(QueryPreparation::CreatingStatement)
    )
}

fn is_creating_reader(state: &QueryState) -> bool {
    matches!(
        state,
        QueryState::Preparing(QueryPreparation::CreatingReader(_))
    )
}

fn is_reading(state: &QueryState) -> bool {
    matches!(state, QueryState::Reading(_))
}

fn finish_operation(
    state: &Mutex<QueryState>,
    is_active: impl FnOnce(&QueryState) -> bool,
    error: Error,
) -> Poll<Option<Result<RecordBatch, Error>>> {
    let mut query_state = match lock_state(state) {
        Ok(query_state) => query_state,
        Err(lock_error) => return Poll::Ready(Some(Err(lock_error))),
    };
    if is_active(&query_state) {
        *query_state = QueryState::Finished;
        Poll::Ready(Some(Err(error)))
    } else if matches!(*query_state, QueryState::Cancelled) {
        *query_state = QueryState::Finished;
        Poll::Ready(Some(Err(cancelled_error())))
    } else {
        Poll::Ready(Some(Err(invalid_state_error(
            "source work failed after its query state changed",
        ))))
    }
}

fn lock_state(state: &Mutex<QueryState>) -> Result<MutexGuard<'_, QueryState>, Error> {
    state
        .lock()
        .map_err(|_| Error::with_message_and_status("query state is poisoned", Status::Internal))
}

fn cancelled_error() -> Error {
    Error::with_message_and_status("query execution was cancelled", Status::Cancelled)
}

fn invalid_state_error(message: impl Into<String>) -> Error {
    Error::with_message_and_status(message, Status::InvalidState)
}
