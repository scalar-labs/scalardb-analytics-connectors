//! Creates one lazy ADBC query execution and exposes its Arrow result as a stream.

use std::{
    marker::PhantomData,
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
use arrow_schema::ArrowError;
use connector_sdk::{ConnectorError, ExecutionCancel};
use futures_core::Stream;

use crate::{AdbcConnection, AdbcConnector, error::stream_fallback_error};

/// Creates a query stream without sending anything to the source.
///
/// The first read prepares an ADBC statement and reader before reading the first
/// batch. Later reads use the same reader and statement. Connector `C`
/// classifies every driver error the query and its cancellation report.
pub fn query<C: AdbcConnector>(
    connection: &AdbcConnection<C>,
    sql: impl Into<String>,
) -> AdbcQueryStream<C> {
    AdbcQueryStream {
        state: Arc::new(Mutex::new(QueryState::Pending(QueryRequest {
            connection: connection.connection.clone(),
            sql: sql.into(),
        }))),
        _connector: PhantomData,
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
pub struct AdbcQueryStream<C> {
    state: Arc<Mutex<QueryState>>,
    _connector: PhantomData<fn() -> C>,
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
}

/// Owns the statement and query arguments used to create the result reader.
struct ReaderCreation {
    sql: String,
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
pub struct AdbcQueryCancelHandle<C> {
    state: Arc<Mutex<QueryState>>,
    _connector: PhantomData<fn() -> C>,
}

impl<C> Clone for AdbcQueryCancelHandle<C> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            _connector: PhantomData,
        }
    }
}

impl<C> AdbcQueryStream<C> {
    /// Returns a cloneable handle for cancelling this query from another thread.
    pub fn cancel_handle(&self) -> AdbcQueryCancelHandle<C> {
        AdbcQueryCancelHandle {
            state: Arc::clone(&self.state),
            _connector: PhantomData,
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

impl<C> AdbcQueryCancelHandle<C> {
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

impl<C: AdbcConnector> CancelHandle for AdbcQueryCancelHandle<C> {
    fn try_cancel(&self) -> Result<(), Error> {
        AdbcQueryCancelHandle::try_cancel(self)
    }
}

impl<C: AdbcConnector> ExecutionCancel for AdbcQueryCancelHandle<C> {
    fn cancel(&self) -> Result<(), ConnectorError> {
        self.try_cancel().map_err(|error| C::classify(error.into()))
    }
}

impl<C: AdbcConnector> Stream for AdbcQueryStream<C> {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let action = this.start_read();
        let read = Read::<C> {
            state: &this.state,
            _connector: PhantomData,
        };
        let stopped = match action {
            Ok(ReadAction::CreateStatement(request)) => {
                return read.prepare_statement_and_read(request);
            }
            Ok(ReadAction::ReadBatch(execution)) => return read.read_batch(execution),
            Ok(ReadAction::ReturnEnd) => return Poll::Ready(None),
            Ok(ReadAction::ReturnCancellation) => read.classify(cancelled_error()),
            Err(error) => read.classify(error),
        };
        this.finish();
        Poll::Ready(Some(Err(stopped)))
    }
}

impl<C> Drop for AdbcQueryStream<C> {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The query state one stream read works with, for Connector `C`.
struct Read<'a, C> {
    state: &'a Mutex<QueryState>,
    _connector: PhantomData<fn() -> C>,
}

impl<C: AdbcConnector> Read<'_, C> {
    fn prepare_statement_and_read(
        &self,
        mut request: QueryRequest,
    ) -> Poll<Option<Result<RecordBatch, ConnectorError>>> {
        let statement = match request.connection.new_statement() {
            Ok(statement) => statement,
            Err(error) => {
                return self.finish_operation(is_creating_statement, self.classify(error));
            }
        };
        let cancel_handle: Arc<dyn CancelHandle> = statement.get_cancel_handle().into();

        let mut query_state = match lock_state(self.state) {
            Ok(query_state) => query_state,
            Err(error) => return Poll::Ready(Some(Err(self.classify(error)))),
        };
        match &*query_state {
            QueryState::Preparing(QueryPreparation::CreatingStatement) => {
                *query_state = QueryState::Preparing(QueryPreparation::CreatingReader(Arc::clone(
                    &cancel_handle,
                )));
                drop(query_state);
                self.prepare_reader_and_read(ReaderCreation {
                    sql: request.sql,
                    statement: StatementResources {
                        cancel_handle,
                        statement,
                    },
                })
            }
            QueryState::Cancelled => {
                *query_state = QueryState::Finished;
                Poll::Ready(Some(Err(self.classify(cancelled_error()))))
            }
            _ => Poll::Ready(Some(Err(self.classify(invalid_state_error(
                "statement creation completed after its query state changed",
            ))))),
        }
    }

    fn prepare_reader_and_read(
        &self,
        mut creation: ReaderCreation,
    ) -> Poll<Option<Result<RecordBatch, ConnectorError>>> {
        if let Err(error) = check_reader_creation(self.state) {
            return Poll::Ready(Some(Err(self.classify(error))));
        }
        if let Err(error) = creation.statement.statement.set_sql_query(&creation.sql) {
            return self.finish_operation(is_creating_reader, self.classify(error));
        }
        if let Err(error) = check_reader_creation(self.state) {
            return Poll::Ready(Some(Err(self.classify(error))));
        }

        let reader = match creation.statement.statement.execute() {
            Ok(reader) => reader,
            Err(error) => {
                return self.finish_operation(is_creating_reader, self.classify(error));
            }
        };
        let execution = QueryExecution {
            reader,
            statement: creation.statement,
        };

        let mut query_state = match lock_state(self.state) {
            Ok(query_state) => query_state,
            Err(error) => return Poll::Ready(Some(Err(self.classify(error)))),
        };
        match &*query_state {
            QueryState::Preparing(QueryPreparation::CreatingReader(_)) => {
                *query_state = QueryState::Reading(Arc::clone(&execution.statement.cancel_handle));
                drop(query_state);
                self.read_batch(execution)
            }
            QueryState::Cancelled => {
                *query_state = QueryState::Finished;
                Poll::Ready(Some(Err(self.classify(cancelled_error()))))
            }
            _ => Poll::Ready(Some(Err(self.classify(invalid_state_error(
                "reader creation completed after its query state changed",
            ))))),
        }
    }

    fn read_batch(
        &self,
        mut execution: QueryExecution,
    ) -> Poll<Option<Result<RecordBatch, ConnectorError>>> {
        if let Err(error) = check_reading(self.state) {
            return Poll::Ready(Some(Err(self.classify(error))));
        }
        let item = execution.reader.next();

        let mut query_state = match lock_state(self.state) {
            Ok(query_state) => query_state,
            Err(error) => return Poll::Ready(Some(Err(self.classify(error)))),
        };
        match &*query_state {
            QueryState::Reading(_) => match item {
                Some(Ok(batch)) => {
                    *query_state = QueryState::Ready(execution);
                    Poll::Ready(Some(Ok(batch)))
                }
                Some(Err(error)) => {
                    *query_state = QueryState::Finished;
                    Poll::Ready(Some(Err(classify_stream_error::<C>(error))))
                }
                None => {
                    *query_state = QueryState::Finished;
                    Poll::Ready(None)
                }
            },
            QueryState::Cancelled => {
                *query_state = QueryState::Finished;
                Poll::Ready(Some(Err(self.classify(cancelled_error()))))
            }
            _ => Poll::Ready(Some(Err(self.classify(invalid_state_error(
                "batch reading completed after its query state changed",
            ))))),
        }
    }

    fn finish_operation(
        &self,
        is_active: impl FnOnce(&QueryState) -> bool,
        error: ConnectorError,
    ) -> Poll<Option<Result<RecordBatch, ConnectorError>>> {
        let mut query_state = match lock_state(self.state) {
            Ok(query_state) => query_state,
            Err(lock_error) => return Poll::Ready(Some(Err(self.classify(lock_error)))),
        };
        if is_active(&query_state) {
            *query_state = QueryState::Finished;
            Poll::Ready(Some(Err(error)))
        } else if matches!(*query_state, QueryState::Cancelled) {
            *query_state = QueryState::Finished;
            Poll::Ready(Some(Err(self.classify(cancelled_error()))))
        } else {
            Poll::Ready(Some(Err(self.classify(invalid_state_error(
                "source work failed after its query state changed",
            )))))
        }
    }

    fn classify(&self, error: Error) -> ConnectorError {
        C::classify(error.into())
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

fn classify_stream_error<C: AdbcConnector>(error: ArrowError) -> ConnectorError {
    // TODO: The Rust ADBC driver manager does not yet import rich errors from
    // ArrowArrayStream. Until the upstream issue below is resolved, mid-stream
    // failures arrive as ArrowError and therefore lack the original ADBC status,
    // SQLSTATE, vendor code, and details:
    // https://github.com/apache/arrow-adbc/issues/4984
    stream_fallback_error::<C>(error.to_string())
}
