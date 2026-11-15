//! One ODBC connection of a Connector's session.

use std::marker::PhantomData;

use arrow_odbc::odbc_api::{Connection, Shared};

/// A session's ODBC connection, typed by the Connector that opened it.
///
/// The Connector type decides how queries on the connection treat ODBC
/// errors. Clones share the connection; each query allocates its own ODBC
/// statement handle on it.
pub struct OdbcConnection<C> {
    pub(crate) connection: Shared<Connection<'static>>,
    _connector: PhantomData<fn() -> C>,
}

impl<C> OdbcConnection<C> {
    pub(crate) fn new(connection: Shared<Connection<'static>>) -> Self {
        Self {
            connection,
            _connector: PhantomData,
        }
    }
}

impl<C> Clone for OdbcConnection<C> {
    fn clone(&self) -> Self {
        Self::new(self.connection.clone())
    }
}
