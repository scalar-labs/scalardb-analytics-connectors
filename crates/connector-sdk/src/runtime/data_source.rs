//! An opened Data Source and the sessions it creates.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{Component, ConnectorError};

use super::{Library, SessionState, connector::DynDataSource, contain_panic, invalid_state};

/// One opened Data Source, shared by its handle and its sessions.
///
/// The connector's Data Source, and the credentials it holds, are released
/// when the handle is closed and every session using it is closed.
pub struct DataSourceState {
    pub(super) library: Arc<Library>,
    /// The name and version of the connector that opened it.
    pub(super) connector: Component,
    inner: Box<dyn DynDataSource>,
    closed: AtomicBool,
}

impl DataSourceState {
    pub(super) fn new(
        library: Arc<Library>,
        connector: Component,
        inner: Box<dyn DynDataSource>,
    ) -> Self {
        Self {
            library,
            connector,
            inner,
            closed: AtomicBool::new(false),
        }
    }

    /// Fails when this Data Source or the library was closed.
    pub(super) fn ensure_open(&self) -> Result<(), ConnectorError> {
        self.library.ensure_open()?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(invalid_state("the data source is closed"));
        }
        Ok(())
    }

    /// Marks the Data Source closed; its sessions' later calls return `invalid state`.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// Opens one session, connecting when the Data Source has connections.
    pub fn open_session(self: &Arc<Self>) -> Result<Arc<SessionState>, ConnectorError> {
        self.ensure_open()?;
        let session = contain_panic(|| self.inner.open_session())?;
        Ok(Arc::new(SessionState::new(Arc::clone(self), session)))
    }
}
