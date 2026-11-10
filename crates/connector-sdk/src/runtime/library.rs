//! The library: the connectors built into it and its timeout watcher.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::{
    DataSourceState,
    connector::{DynConnectorLibrary, create_library},
    contain_panic, invalid_state,
    watcher::Watcher,
};
use crate::{
    ConnectionOptions, ConnectorDescriptor, ConnectorError, ConnectorLibrary, DataSource,
    DataSourceSession, ErrorKind, LibrarySettings, PortableQuery, SecretString,
};

/// One connector built into a library, registered before initialization.
pub struct BuiltInConnector {
    /// What the connector declares; available before initialization.
    pub descriptor: ConnectorDescriptor,
    create: fn(&LibrarySettings) -> Result<Box<dyn DynConnectorLibrary>, ConnectorError>,
}

impl BuiltInConnector {
    /// Registers connector library `L`.
    pub fn of<L>() -> Self
    where
        L: ConnectorLibrary,
        <<L::DataSource as DataSource>::Session as DataSourceSession>::Query:
            PortableQuery + Send + Sync + 'static,
    {
        Self {
            descriptor: L::descriptor(),
            create: create_library::<L>,
        }
    }
}

/// An initialized library, shared by every Data Source opened from it.
///
/// Its state, including the timeout watcher, lives until its last Data
/// Source, session, and operation are released, so reads running when the
/// library is shut down still stop at their deadlines.
pub struct Library {
    connectors: Vec<(ConnectorDescriptor, Box<dyn DynConnectorLibrary>)>,
    pub(super) watcher: Watcher,
    closed: AtomicBool,
}

impl Library {
    /// Creates every connector's process-level state and starts the watcher.
    pub fn initialize(
        connectors: &[BuiltInConnector],
        settings: &LibrarySettings,
    ) -> Result<Arc<Self>, ConnectorError> {
        let connectors = connectors
            .iter()
            .map(|connector| {
                let library = contain_panic(|| (connector.create)(settings))?;
                Ok((connector.descriptor.clone(), library))
            })
            .collect::<Result<_, ConnectorError>>()?;
        Ok(Arc::new(Self {
            connectors,
            watcher: Watcher::start()?,
            closed: AtomicBool::new(false),
        }))
    }

    /// Shuts the library down; calls on its Data Sources and their children
    /// return `invalid state` from now on.
    pub fn shut_down(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub(super) fn ensure_open(&self) -> Result<(), ConnectorError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(invalid_state("the library was shut down"));
        }
        Ok(())
    }

    /// Opens a Data Source through the connector that handles `connector`.
    ///
    /// The options are validated against that connector's descriptor first.
    pub fn open_data_source(
        self: &Arc<Self>,
        connector: &str,
        options: Vec<(String, SecretString)>,
        connection_timeout: Option<Duration>,
    ) -> Result<Arc<DataSourceState>, ConnectorError> {
        self.ensure_open()?;
        let (descriptor, library) = self
            .connectors
            .iter()
            .find(|(descriptor, _)| descriptor.connectors.iter().any(|name| name == connector))
            .ok_or_else(|| {
                ConnectorError::new(
                    ErrorKind::InvalidSettings,
                    false,
                    format!("Connector `{connector}` is not built into this library"),
                )
            })?;
        let options = ConnectionOptions::parse(descriptor, options)?;
        let data_source =
            contain_panic(|| library.open_data_source(connector, options, connection_timeout))?;
        Ok(Arc::new(DataSourceState::new(
            Arc::clone(self),
            data_source,
        )))
    }
}
