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
    Component, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError, ErrorKind,
    LibrarySettings, SecretString,
};

/// One connector built into a library, registered before initialization.
pub struct BuiltInConnector {
    /// What the connector declares; available before initialization.
    pub descriptor: ConnectorDescriptor,
    create: fn(&LibrarySettings) -> Result<Box<dyn DynConnectorLibrary>, ConnectorError>,
}

impl BuiltInConnector {
    /// Registers Connector `C`.
    pub fn of<C: Connector>() -> Self {
        Self {
            descriptor: C::descriptor(),
            create: create_library::<C>,
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
    /// The library version, reported in retrieval info.
    pub(super) version: String,
    pub(super) watcher: Watcher,
    closed: AtomicBool,
}

impl Library {
    /// Creates every connector's process-level state and starts the watcher.
    ///
    /// `version` is the version of the library these connectors are built
    /// into, as its static information states.
    pub fn initialize(
        connectors: &[BuiltInConnector],
        settings: &LibrarySettings,
        version: &str,
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
            version: version.to_owned(),
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
        let connector = Component {
            name: descriptor.name.clone(),
            version: descriptor.version.clone(),
        };
        Ok(Arc::new(DataSourceState::new(
            Arc::clone(self),
            connector,
            data_source,
        )))
    }
}
