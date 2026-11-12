//! One ADBC connection of a Connector's session.

use std::marker::PhantomData;

use adbc_driver_manager::ManagedConnection;
use connector_sdk::Component;

/// A session's ADBC connection, typed by the Connector whose driver it reaches.
///
/// The Connector type decides how queries on the connection treat driver
/// errors. Clones share the connection.
pub struct AdbcConnection<C> {
    pub(crate) connection: ManagedConnection,
    _connector: PhantomData<fn() -> C>,
}

impl<C> AdbcConnection<C> {
    /// Wraps a driver-manager connection to Connector `C`'s driver.
    ///
    /// [`crate::AdbcDatabase`] creates its sessions' connections this way.
    pub fn new(connection: ManagedConnection) -> Self {
        Self {
            connection,
            _connector: PhantomData,
        }
    }
}

impl<C> Clone for AdbcConnection<C> {
    fn clone(&self) -> Self {
        Self::new(self.connection.clone())
    }
}

/// One session of an ADBC-access Connector: its connection and the driver it
/// reaches.
pub struct AdbcSession<C> {
    connection: AdbcConnection<C>,
    driver: Component,
}

impl<C> AdbcSession<C> {
    /// Pairs a session's connection with the driver it reaches.
    pub fn new(connection: AdbcConnection<C>, driver: Component) -> Self {
        Self { connection, driver }
    }

    /// The session's connection.
    pub fn connection(&self) -> &AdbcConnection<C> {
        &self.connection
    }

    /// The driver's name and version, as retrieval info records them.
    pub fn driver(&self) -> &Component {
        &self.driver
    }
}
