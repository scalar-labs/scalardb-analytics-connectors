//! ADBC access: what a Connector using it declares, and the access built
//! from that declaration.

use std::{
    marker::PhantomData,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use adbc_core::options::{OptionDatabase, OptionValue};
use connector_sdk::{
    Access, Component, ConnectionOptions, Connector, ConnectorError, DataSourceAccess, ErrorClass,
    LibrarySettings,
};

use crate::{
    AdbcDiagnostic, AdbcSession, DriverRequirement, data_source::AdbcDatabase, driver::AdbcDriver,
    error,
};

/// A Connector that reaches its Data Source through an ADBC driver.
///
/// Its access is [`Adbc`], which loads the declared driver, opens one ADBC
/// database per Data Source and one connection per session, checks the driver
/// version, and builds errors from the driver's diagnostics. The Connector's
/// error handler sees [`AdbcDiagnostic`]s.
pub trait AdbcConnector: Connector<Access = Adbc<Self>> {
    /// The ADBC driver this Connector uses and the versions it was tested with.
    const DRIVER: DriverRequirement;

    /// Turns validated connection options into the driver's database options,
    /// adding the Connector's fixed driver options and session settings.
    ///
    /// `connection_timeout` limits connecting; `None` means no limit.
    fn database_options(
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Vec<(OptionDatabase, OptionValue)>, ConnectorError>;
}

/// ADBC access for Connector `C`.
pub struct Adbc<C>(PhantomData<fn() -> C>);

impl<C: AdbcConnector> Access<C> for Adbc<C> {
    type Library = AdbcLibrary;
    type DataSource = AdbcDatabase<C>;
    type Session = AdbcSession<C>;
    type Diagnostic = AdbcDiagnostic;

    fn description() -> DataSourceAccess {
        C::DRIVER.access()
    }

    fn open_library(settings: &LibrarySettings) -> Result<AdbcLibrary, ConnectorError> {
        Ok(AdbcLibrary {
            settings: settings.clone(),
            driver: Mutex::new(None),
        })
    }

    fn open_data_source(
        library: &AdbcLibrary,
        _connector: &str,
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<AdbcDatabase<C>, ConnectorError> {
        let options = C::database_options(options, connection_timeout)?;
        AdbcDatabase::open(&library.driver(C::DRIVER)?, options)
    }

    fn open_session(database: &AdbcDatabase<C>) -> Result<AdbcSession<C>, ConnectorError> {
        database.connect()
    }

    fn component(session: &AdbcSession<C>) -> Option<Component> {
        Some(session.driver().clone())
    }

    fn error(diagnostic: AdbcDiagnostic, class: Option<ErrorClass>) -> ConnectorError {
        error::to_connector_error(diagnostic, class)
    }
}

/// ADBC access's process-level state: the library settings and the driver,
/// loaded when the first Data Source opens.
///
/// A library that never opens a Data Source does not need the driver
/// installed. A failed load is not kept, so a later open tries again.
pub struct AdbcLibrary {
    settings: LibrarySettings,
    driver: Mutex<Option<AdbcDriver>>,
}

impl AdbcLibrary {
    fn driver(&self, requirement: DriverRequirement) -> Result<AdbcDriver, ConnectorError> {
        let mut driver = self.driver.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(driver) = driver.as_ref() {
            return Ok(driver.clone());
        }
        let loaded = requirement.load(&self.settings)?;
        *driver = Some(loaded.clone());
        Ok(loaded)
    }
}
