//! One operation, a read or a metadata request: lazy start, pulls,
//! cancellation, and its error.

use std::{
    mem,
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
    time::{Duration, Instant},
};

use crate::{ConnectorError, ErrorKind, ExecutionCancelHandle, RecordBatchStream};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;

use super::{
    MetadataRequest, SessionState, SharedRemoteExecution, invalid_state, lock, panic_error,
    session::SessionLease, watcher::WatchRegistration,
};

/// What an operation asks of the Data Source.
pub(super) enum OperationRequest {
    /// Reads the rows of a planned remote execution.
    Read(SharedRemoteExecution),
    /// Reads the catalog for one metadata request.
    Metadata(MetadataRequest),
}

/// The batches of a started operation and the means of stopping it.
pub(super) struct StartedStream {
    pub(super) batches: RecordBatchStream,
    pub(super) cancel: ExecutionCancelHandle,
}

/// The Data Source resources of an operation.
///
/// The session lease is held from the start of the operation until its stream
/// ends, is released, or the operation is closed.
enum OperationState {
    /// Started but not yet pulled; nothing was sent to the Data Source.
    Pending(SessionLease),
    Running {
        stream: RecordBatchStream,
        _lease: SessionLease,
        /// Present when the operation has a timeout.
        _deadline: Option<WatchRegistration>,
    },
    /// Ended, released, or closed; every Data Source resource is released.
    Finished,
}

/// What a cancellation does in each phase of the operation.
enum CancelState {
    /// Nothing was sent yet; a cancellation is only recorded.
    Pending,
    /// Cancelled before the request was sent; starting it returns `cancellation`.
    Requested,
    Running(ExecutionCancelHandle),
    Finished,
}

/// One operation, shared by its handle and its result stream.
///
/// Reads and metadata requests follow the same rules: the request is sent at
/// the first pull, the session runs no other operation until the stream ends
/// or is released, and cancellation, the timeout, and panics are handled alike.
pub struct Operation {
    session: Arc<SessionState>,
    request: OperationRequest,
    timeout: Option<Duration>,
    closed: AtomicBool,
    /// Held during a pull; the caller never pulls from several threads at once.
    state: Mutex<OperationState>,
    /// Separate from `read` so that cancellation never waits for a blocked pull.
    cancel: Mutex<CancelState>,
    /// The first error a pull returned, kept until the operation is closed.
    error: OnceLock<ConnectorError>,
}

impl Operation {
    pub(super) fn new(
        lease: SessionLease,
        request: OperationRequest,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            session: Arc::clone(&lease.session),
            request,
            timeout,
            closed: AtomicBool::new(false),
            state: Mutex::new(OperationState::Pending(lease)),
            cancel: Mutex::new(CancelState::Pending),
            error: OnceLock::new(),
        }
    }

    /// Returns the schema of every batch, known before the Data Source is queried.
    pub fn output_schema(&self) -> SchemaRef {
        match &self.request {
            OperationRequest::Read(remote) => remote.output_schema(),
            OperationRequest::Metadata(request) => request.output_schema(),
        }
    }

    /// Returns the error that ended the operation's stream, if any.
    pub fn error(&self) -> Option<&ConnectorError> {
        self.error.get()
    }

    fn ensure_open(&self) -> Result<(), ConnectorError> {
        self.session.ensure_open()?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(invalid_state("the operation is closed"));
        }
        Ok(())
    }

    /// Pulls the next batch; `None` means the stream ended.
    ///
    /// The first pull sends the request. A panic in the connector becomes an
    /// `internal` error, releases the operation, and marks the session unusable.
    pub fn pull(&self) -> Result<Option<RecordBatch>, ConnectorError> {
        match panic::catch_unwind(AssertUnwindSafe(|| self.pull_contained())) {
            Ok(result) => result,
            Err(_) => {
                self.session.mark_unusable();
                self.release();
                Err(self.record(panic_error()))
            }
        }
    }

    fn pull_contained(&self) -> Result<Option<RecordBatch>, ConnectorError> {
        let mut read = lock(&self.state);
        if let Err(error) = self.ensure_open() {
            self.finish(&mut read);
            return Err(self.record(error));
        }
        if matches!(*read, OperationState::Pending(_)) {
            if let Err(error) = self.start(&mut read) {
                self.finish(&mut read);
                return Err(self.record(error));
            }
        }
        let OperationState::Running { stream, .. } = &mut *read else {
            return Ok(None);
        };
        match next_blocking(stream).map(|item| self.session.observe(|| item)) {
            Some(Ok(batch)) => Ok(Some(batch)),
            Some(Err(error)) => {
                self.finish(&mut read);
                Err(self.record(error))
            }
            None => {
                self.finish(&mut read);
                Ok(None)
            }
        }
    }

    /// Sends the request: starts it, its deadline, and its cancellation.
    fn start(&self, read: &mut OperationState) -> Result<(), ConnectorError> {
        let OperationState::Pending(lease) = mem::replace(read, OperationState::Finished) else {
            unreachable!("only a pending operation starts");
        };
        if matches!(*lock(&self.cancel), CancelState::Requested) {
            return Err(ConnectorError::new(
                ErrorKind::Cancellation,
                false,
                "connector execution was cancelled",
            ));
        }
        let deadline = self.timeout.map(|timeout| Instant::now() + timeout);
        let StartedStream { batches, cancel } = self
            .session
            .observe(|| self.session.start(&self.request, deadline))?;
        let registration = deadline.map(|deadline| {
            self.session
                .library()
                .watcher
                .register(deadline, cancel.clone())
        });
        let mut cancel_state = lock(&self.cancel);
        if matches!(*cancel_state, CancelState::Requested) {
            // Cancelled while the request started: the stop is recorded
            // before the connector is asked to stop, so the next pull returns
            // `cancellation` whatever this call returns.
            let _ = cancel.cancel();
        }
        *cancel_state = CancelState::Running(cancel);
        *read = OperationState::Running {
            stream: batches,
            _lease: lease,
            _deadline: registration,
        };
        Ok(())
    }

    /// Cancels the operation from any thread.
    pub fn cancel(&self) -> Result<(), ConnectorError> {
        self.ensure_open()?;
        let cancel = {
            let mut state = lock(&self.cancel);
            match &*state {
                CancelState::Pending => {
                    *state = CancelState::Requested;
                    return Ok(());
                }
                CancelState::Requested | CancelState::Finished => return Ok(()),
                CancelState::Running(cancel) => cancel.clone(),
            }
        };
        cancel.cancel()
    }

    /// Releases the operation: stops the Data Source request and returns the lease.
    ///
    /// A panic while the connector releases its resources is contained here,
    /// because release runs from C callbacks that cannot unwind.
    pub fn release(&self) {
        let read = mem::replace(&mut *lock(&self.state), OperationState::Finished);
        *lock(&self.cancel) = CancelState::Finished;
        if panic::catch_unwind(AssertUnwindSafe(|| drop(read))).is_err() {
            self.session.mark_unusable();
        }
    }

    /// Closes the operation, releasing every Data Source resource.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.release();
    }

    fn finish(&self, read: &mut OperationState) {
        *read = OperationState::Finished;
        *lock(&self.cancel) = CancelState::Finished;
    }

    /// Keeps the first error for retrieval and returns it to the caller.
    fn record(&self, error: ConnectorError) -> ConnectorError {
        // The stream ends at its first error, so a later error, such as from a
        // pull after the end, does not replace the one that ended it.
        let _ = self.error.set(error.clone());
        error
    }
}

/// Polls the stream on the caller's thread until it yields.
///
/// Data Source access is synchronous; a stream that returns `Pending` wakes
/// this thread when it can make progress.
fn next_blocking(stream: &mut RecordBatchStream) -> Option<Result<RecordBatch, ConnectorError>> {
    struct ThreadWaker(Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        match stream.as_mut().poll_next(&mut context) {
            Poll::Ready(item) => return item,
            Poll::Pending => thread::park(),
        }
    }
}
