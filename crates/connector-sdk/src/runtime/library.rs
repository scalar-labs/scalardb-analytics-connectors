//! The library: the connectors built into it and its timeout watcher.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use schemars::{Schema, generate::SchemaSettings};

use super::{
    DataSourceState, SourceMetadata,
    connector::{DynConnectorLibrary, create_library},
    contain_panic, invalid_state,
    watcher::Watcher,
};
use crate::{
    Catalog, Component, ConnectionOptions, Connector, ConnectorDescriptor, ConnectorError,
    ErrorKind, JsonDifference, LibrarySettings, SecretString,
};

/// One connector built into a library, registered before initialization.
pub struct BuiltInConnector {
    /// What the connector declares; available before initialization.
    pub descriptor: ConnectorDescriptor,
    /// The JSON Schema (draft 2020-12) of the source metadata documents the
    /// connector produces, derived from how its [`Catalog::Table`]
    /// serializes; available before initialization.
    pub source_metadata_schema: Schema,
    create: fn(&LibrarySettings) -> Result<Box<dyn DynConnectorLibrary>, ConnectorError>,
}

impl BuiltInConnector {
    /// Registers Connector `C`.
    pub fn of<C: Connector>() -> Self {
        Self {
            descriptor: C::descriptor(),
            source_metadata_schema: SchemaSettings::draft2020_12()
                .for_serialize()
                .into_generator()
                .into_root_schema_for::<<C::Catalog as Catalog<C>>::Table>(),
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

    /// Compares two observations of one table's source metadata as JSON
    /// documents; `None` when they do not differ.
    ///
    /// The connector that produced both documents restores them. Documents of
    /// different connectors, of a connector not built into this library, or
    /// that the connector cannot restore are invalid requests. The table
    /// metadata of the two observations is compared on its own
    /// ([`crate::TableMetadata::compare`]).
    pub fn compare_source_metadata(
        &self,
        before: &SourceMetadata,
        after: &SourceMetadata,
    ) -> Result<Option<Vec<JsonDifference>>, ConnectorError> {
        self.ensure_open()?;
        if before.connector != after.connector {
            return Err(ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "the source metadata documents are of different connectors",
            ));
        }
        let (_, library) = self
            .connectors
            .iter()
            .find(|(descriptor, _)| descriptor.name == before.connector)
            .ok_or_else(|| {
                ConnectorError::new(
                    ErrorKind::InvalidRequest,
                    false,
                    format!(
                        "connector `{}` is not built into this library",
                        before.connector
                    ),
                )
            })?;
        contain_panic(|| library.compare_source_metadata(&before.document, &after.document))
    }
}
