//! A session, its one-operation-at-a-time lease, and its usability.

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    ConnectorError, ErrorKind, Listed, NameFilter, NamespacePath, SourceStream, TableEntry,
    TableReference,
};

use super::{
    Cancellation, DataSourceState, Operation, PlanningOutcome, Retrieval, SharedRemoteExecution,
    TableMetadataResult, connector::DynSession, invalid_state, lock, next_blocking,
    operation::StartedStream, panic_error,
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
        Ok(Arc::new(Operation::new(lease, remote, timeout)))
    }

    /// Lists the namespaces whose names exactly match `namespaces`, or all of them.
    pub fn namespace_list(
        self: &Arc<Self>,
        namespaces: &NameFilter,
        timeout: Option<Duration>,
        cancellation: Option<&Cancellation>,
    ) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
        self.read_catalog(timeout, cancellation, |deadline| {
            self.inner.namespace_list(namespaces, deadline)
        })
    }

    /// Lists the tables of the matching namespaces whose names exactly match
    /// `tables`, or all of them.
    pub fn table_list(
        self: &Arc<Self>,
        namespaces: &NameFilter,
        tables: &NameFilter,
        timeout: Option<Duration>,
        cancellation: Option<&Cancellation>,
    ) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
        self.read_catalog(timeout, cancellation, |deadline| {
            self.inner.table_list(namespaces, tables, deadline)
        })
    }

    /// Reads one record per requested table, in request order.
    pub fn table_metadata(
        self: &Arc<Self>,
        tables: &[TableReference],
        timeout: Option<Duration>,
        cancellation: Option<&Cancellation>,
    ) -> Result<TableMetadataResult, ConnectorError> {
        let connector = &self.data_source.connector;
        let records = self.read_catalog(timeout, cancellation, |deadline| {
            self.inner.table_metadata(tables, &connector.name, deadline)
        })?;
        Ok(TableMetadataResult {
            records,
            retrieval: Retrieval {
                connector: connector.clone(),
                adbc_driver: self.inner.adbc_driver(),
                library_version: self.library().version.clone(),
            },
        })
    }

    /// Runs one catalog read to its end while holding the session's lease.
    ///
    /// The timeout counts from the call. A cancellation given before the call
    /// returns `cancellation` without sending anything to the Data Source. The
    /// first error ends the read and is the result; nothing read before it is
    /// returned.
    fn read_catalog<T>(
        self: &Arc<Self>,
        timeout: Option<Duration>,
        cancellation: Option<&Cancellation>,
        start: impl FnOnce(Option<Instant>) -> Result<SourceStream<T>, ConnectorError>,
    ) -> Result<Vec<T>, ConnectorError> {
        let _lease = self.lease()?;
        if let Some(cancellation) = cancellation {
            cancellation.ensure_not_cancelled()?;
        }
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        self.observe(|| {
            let mut items = start(deadline)?;
            let handle = items.cancel_handle();
            let _deadline =
                deadline.map(|deadline| self.library().watcher.register(deadline, handle.clone()));
            let _cancellation = cancellation
                .map(|cancellation| cancellation.register(handle))
                .transpose()?;
            let mut read = Vec::new();
            while let Some(item) = next_blocking(&mut items) {
                read.push(item?);
            }
            Ok(read)
        })
    }

    /// Sends a read's remote execution to the connector.
    pub(super) fn start(
        &self,
        remote: &SharedRemoteExecution,
        deadline: Option<Instant>,
    ) -> Result<StartedStream, ConnectorError> {
        let stream = self.inner.execute(remote, deadline)?;
        Ok(StartedStream {
            cancel: stream.cancel_handle(),
            batches: Box::pin(stream),
        })
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
