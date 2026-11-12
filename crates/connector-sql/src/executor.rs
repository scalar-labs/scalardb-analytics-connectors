//! Sending a planned SQL read's statement to the Data Source.

use std::{marker::PhantomData, sync::Arc};

use connector_adbc::query;
use connector_sdk::{ConnectorError, ExecutionOutput, Executor, Session};

use crate::{SqlConnector, SqlRead};

/// The executor of a SQL-speaking Connector: one statement on the session's
/// connection.
///
/// The SDK checks the registered schema before the statement and converts its
/// batches afterwards.
pub struct SqlExecutor<C>(PhantomData<fn() -> C>);

impl<C: SqlConnector> Executor<C> for SqlExecutor<C> {
    fn run(session: &Session<C>, read: &SqlRead) -> Result<ExecutionOutput, ConnectorError> {
        let statement = query(session.access().connection(), read.sql.clone());
        let cancel = Arc::new(statement.cancel_handle());
        Ok(ExecutionOutput::new(Box::pin(statement), cancel))
    }
}
