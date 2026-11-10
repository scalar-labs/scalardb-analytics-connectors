//! A session, its one-operation-at-a-time lease, and its usability.

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{ConnectorError, ErrorKind};

use super::{
    DataSourceState, MetadataRequest, Operation, PlanningOutcome, SharedRemoteExecution,
    connector::DynSession,
    invalid_state, lock,
    metadata::Retrieval,
    operation::{OperationRequest, StartedStream},
    panic_error,
};

/// Whether a session accepts requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionCondition {
    Usable,
    /// The connection state is unknown after a broken connection or a panic;
    /// the caller closes and reopens the session.
    Unusable,
    Closed,
}

/// One session, shared by its handle and its operations.
pub struct SessionState {
    data_source: Arc<DataSourceState>,
    pub(super) inner: Box<dyn DynSession>,
    condition: Mutex<SessionCondition>,
    /// Whether an operation holds the session's lease.
    leased: AtomicBool,
}

impl SessionState {
    pub(super) fn new(data_source: Arc<DataSourceState>, inner: Box<dyn DynSession>) -> Self {
        Self {
            data_source,
            inner,
            condition: Mutex::new(SessionCondition::Usable),
            leased: AtomicBool::new(false),
        }
    }

    pub(super) fn library(&self) -> &super::Library {
        &self.data_source.library
    }

    /// Fails when this session or one of its ancestors was closed.
    pub(super) fn ensure_open(&self) -> Result<(), ConnectorError> {
        self.data_source.ensure_open()?;
        if *lock(&self.condition) == SessionCondition::Closed {
            return Err(invalid_state("the session is closed"));
        }
        Ok(())
    }

    /// Fails when the session cannot accept a new request.
    fn ensure_usable(&self) -> Result<(), ConnectorError> {
        self.ensure_open()?;
        if *lock(&self.condition) == SessionCondition::Unusable {
            return Err(invalid_state(
                "the session is unusable; close it and open a new one",
            ));
        }
        Ok(())
    }

    /// Records that the connection state is unknown, unless the session is closed.
    pub(super) fn mark_unusable(&self) {
        let mut condition = lock(&self.condition);
        if *condition == SessionCondition::Usable {
            *condition = SessionCondition::Unusable;
        }
    }

    /// Runs connector code that may use the session's Data Source access.
    ///
    /// A broken connection or a panic leaves the connection state unknown, so
    /// either marks the session unusable; a panic becomes an `internal` error.
    pub(super) fn observe<T>(
        &self,
        call: impl FnOnce() -> Result<T, ConnectorError>,
    ) -> Result<T, ConnectorError> {
        let result = panic::catch_unwind(AssertUnwindSafe(call)).unwrap_or_else(|_| {
            self.mark_unusable();
            Err(panic_error())
        });
        if let Err(error) = &result {
            if error.kind() == ErrorKind::Connection {
                self.mark_unusable();
            }
        }
        result
    }

    /// Marks the session closed; its operations' later calls return `invalid state`.
    pub fn close(&self) {
        *lock(&self.condition) = SessionCondition::Closed;
    }

    /// Takes the lease that lets one request use the session.
    fn lease(self: &Arc<Self>) -> Result<SessionLease, ConnectorError> {
        self.ensure_usable()?;
        if self
            .leased
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(invalid_state(
                "the session is running another operation; finish or release its stream first",
            ));
        }
        Ok(SessionLease {
            session: Arc::clone(self),
        })
    }

    /// Plans one pushdown request while holding the session's lease.
    pub fn plan_pushdown(
        self: &Arc<Self>,
        offered_plan: &[u8],
    ) -> Result<PlanningOutcome, ConnectorError> {
        let _lease = self.lease()?;
        self.observe(|| self.inner.plan_pushdown(offered_plan))
    }

    /// Restores a remote execution carried from another process.
    pub fn restore_remote_execution(
        &self,
        bytes: &[u8],
    ) -> Result<SharedRemoteExecution, ConnectorError> {
        self.ensure_usable()?;
        self.observe(|| self.inner.restore_remote_execution(bytes))
    }

    /// Starts a read; the query is sent at the operation's first pull.
    pub fn start_read(
        self: &Arc<Self>,
        remote: SharedRemoteExecution,
        timeout: Option<Duration>,
    ) -> Result<Arc<Operation>, ConnectorError> {
        let lease = self.lease()?;
        self.inner.check_remote_execution(&remote)?;
        Ok(Arc::new(Operation::new(
            lease,
            OperationRequest::Read(remote),
            timeout,
        )))
    }

    /// Starts a metadata request; the catalog is read from the operation's
    /// first pull.
    pub fn start_metadata(
        self: &Arc<Self>,
        request: MetadataRequest,
        timeout: Option<Duration>,
    ) -> Result<Arc<Operation>, ConnectorError> {
        let lease = self.lease()?;
        Ok(Arc::new(Operation::new(
            lease,
            OperationRequest::Metadata(request),
            timeout,
        )))
    }

    /// Sends an operation's request to the connector.
    pub(super) fn start(
        &self,
        request: &OperationRequest,
        deadline: Option<Instant>,
    ) -> Result<StartedStream, ConnectorError> {
        match request {
            OperationRequest::Read(remote) => {
                let stream = self.inner.execute(remote, deadline)?;
                Ok(StartedStream {
                    cancel: stream.cancel_handle(),
                    batches: Box::pin(stream),
                })
            }
            OperationRequest::Metadata(request) => {
                let retrieval = Retrieval {
                    connector: self.data_source.connector.clone(),
                    adbc_driver: self.inner.adbc_driver(),
                    library_version: self.library().version.clone(),
                };
                self.inner.start_metadata(request, retrieval, deadline)
            }
        }
    }
}

/// The right of one request to use a session, returned when dropped.
pub(crate) struct SessionLease {
    pub(super) session: Arc<SessionState>,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.session.leased.store(false, Ordering::SeqCst);
    }
}
