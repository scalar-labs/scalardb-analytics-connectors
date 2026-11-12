//! The query of a SQL-speaking Connector: one statement.

use connector_sdk::{ConnectorError, ErrorKind, PortableQuery};

/// One non-parameterized statement implementing a remote plan.
///
/// The SDK's planned read carries the table and schemas around it. It
/// intentionally does not implement `Debug`: the SQL contains filter literals.
#[derive(Clone, PartialEq, Eq)]
pub struct SqlRead {
    pub sql: String,
}

impl PortableQuery for SqlRead {
    fn to_bytes(&self) -> Vec<u8> {
        self.sql.as_bytes().to_vec()
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError> {
        let sql = String::from_utf8(bytes.to_vec()).map_err(|_| {
            ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "the bytes are not a SQL statement",
            )
        })?;
        Ok(Self { sql })
    }
}
