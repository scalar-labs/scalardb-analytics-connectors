//! A Data Source opened through an ADBC driver: one ADBC database.

use std::{collections::HashSet, marker::PhantomData, sync::OnceLock};

use adbc_core::{
    Connection, Database, Driver,
    options::{InfoCode, OptionDatabase, OptionValue},
};
use adbc_driver_manager::{ManagedConnection, ManagedDatabase};
use arrow_array::{Array, RecordBatch, StringArray, UInt32Array, UnionArray};
use connector_sdk::{Component, ConnectorError, ErrorKind};

use crate::{
    AdbcConnection, AdbcConnector, AdbcSession, DriverVersion,
    driver::{AdbcDriver, invalid_settings},
    error::stream_fallback_error,
};

/// The version text a driver reports when it does not know its own version.
const UNKNOWN_VERSION: &str = "unknown";

/// One ADBC database shared by the sessions of a Data Source.
///
/// The driver manager serializes calls on the database, so sessions can
/// connect from several threads. The driver version is checked when the first
/// session connects, because ADBC reports it only through a connection, unless
/// the driver's manifest declares it.
pub struct AdbcDatabase<C> {
    database: ManagedDatabase,
    driver: AdbcDriver,
    /// The driver name and version, once the first connection identified them.
    identified: OnceLock<Component>,
    _connector: PhantomData<fn() -> C>,
}

impl<C: AdbcConnector> AdbcDatabase<C> {
    /// Opens a database with the Connector's driver options.
    ///
    /// The driver manager takes option values as owned strings, so a secret
    /// value given here is copied out of its secret-holding type and is not
    /// erased by the connector; the driver keeps its own copy while the
    /// database is alive.
    pub(crate) fn open(
        driver: &AdbcDriver,
        options: Vec<(OptionDatabase, OptionValue)>,
    ) -> Result<Self, ConnectorError> {
        let database = driver
            .driver
            .clone()
            .new_database_with_opts(options)
            .map_err(|error| C::classify(error.into()))?;
        Ok(Self {
            database,
            driver: driver.clone(),
            identified: OnceLock::new(),
            _connector: PhantomData,
        })
    }

    /// Identifies the driver and checks the supported range.
    ///
    /// A driver whose manifest declares its name and version is identified by
    /// the manifest without asking the driver. `GetInfo` is not safe to call
    /// concurrently on every driver: the ADBC Driver Foundry MySQL driver 0.8.0
    /// writes the information that all its connections share on each call,
    /// without a lock, and Go ends the process when the write meets another
    /// connection's.
    fn identify(&self, connection: &ManagedConnection) -> Result<Component, ConnectorError> {
        let (name, version) = match (&self.driver.manifest_name, &self.driver.manifest_version) {
            (Some(name), Some(version)) => (name.clone(), version.clone()),
            _ => self.reported(connection)?,
        };
        let parsed = version.parse::<DriverVersion>().map_err(|()| {
            invalid_settings(format!(
                "the ADBC driver version `{version}` is not a release version"
            ))
        })?;
        let requirement = self.driver.requirement;
        if !requirement.supports(parsed) {
            return Err(invalid_settings(format!(
                "the ADBC driver version {version} is outside the supported range {}",
                requirement.supported_versions()
            )));
        }
        Ok(Component { name, version })
    }

    /// Reads the name and version the driver reports through `GetInfo`.
    ///
    /// A driver that reports its version as `unknown`, as the Apache Arrow ADBC
    /// PostgreSQL driver does, is identified by its manifest's version.
    fn reported(&self, connection: &ManagedConnection) -> Result<(String, String), ConnectorError> {
        let codes = HashSet::from([InfoCode::DriverName, InfoCode::DriverVersion]);
        let reader = connection
            .get_info(Some(codes))
            .map_err(|error| C::classify(error.into()))?;
        let mut name = None;
        let mut version = None;
        for batch in reader {
            // A failed pull is the driver's error, not unreadable contents.
            let batch = batch.map_err(|error| stream_fallback_error::<C>(error.to_string()))?;
            for (code, value) in info_strings(&batch)? {
                if code == u32::from(&InfoCode::DriverName) {
                    name = Some(value);
                } else if code == u32::from(&InfoCode::DriverVersion) {
                    version = Some(value);
                }
            }
        }
        let (Some(name), Some(version)) = (name, version) else {
            return Err(unreadable_driver_info());
        };
        let version = match (version.as_str(), &self.driver.manifest_version) {
            (UNKNOWN_VERSION, Some(declared)) => declared.clone(),
            _ => version,
        };
        Ok((name, version))
    }
}

impl<C: AdbcConnector> AdbcDatabase<C> {
    /// Connects one session, checking the driver version on the first
    /// connection.
    pub(crate) fn connect(&self) -> Result<AdbcSession<C>, ConnectorError> {
        let connection = self
            .database
            .new_connection()
            .map_err(|error| C::classify(error.into()))?;
        let driver = match self.identified.get() {
            Some(driver) => driver.clone(),
            None => {
                let driver = self.identify(&connection)?;
                self.identified.get_or_init(|| driver).clone()
            }
        };
        Ok(AdbcSession::new(AdbcConnection::new(connection), driver))
    }
}

/// Returns the string-valued entries of one `GetInfo` batch.
fn info_strings(batch: &RecordBatch) -> Result<Vec<(u32, String)>, ConnectorError> {
    let codes = batch
        .column(0)
        .as_any()
        .downcast_ref::<UInt32Array>()
        .ok_or_else(unreadable_driver_info)?;
    let values = batch
        .column(1)
        .as_any()
        .downcast_ref::<UnionArray>()
        .ok_or_else(unreadable_driver_info)?;
    let mut entries = Vec::new();
    for row in 0..batch.num_rows() {
        let value = values.value(row);
        if let Some(text) = value.as_any().downcast_ref::<StringArray>()
            && text.is_valid(0)
        {
            entries.push((codes.value(row), text.value(0).to_owned()));
        }
    }
    Ok(entries)
}

fn unreadable_driver_info() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Internal,
        false,
        "the ADBC driver returned unreadable driver information",
    )
}
