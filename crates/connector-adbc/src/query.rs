//! Pull-driven execution of one ADBC query.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use adbc_core::{Connection, Statement, error::Error};
use adbc_driver_manager::{ManagedConnection, ManagedStatement};
use arrow_array::{RecordBatch, RecordBatchReader};
use futures_core::Stream;

/// Creates a query stream without creating or executing an ADBC statement.
///
/// The first pull creates the statement, sets `sql`, binds `parameters` when
/// present, and executes it. Later pulls consume that same execution.
pub fn query(
    connection: ManagedConnection,
    sql: impl Into<String>,
    parameters: Option<RecordBatch>,
) -> AdbcQueryStream {
    AdbcQueryStream {
        connection,
        sql: sql.into(),
        parameters,
        started: false,
        execution: None,
    }
}

/// A pull-based Arrow stream for one ADBC query execution.
#[must_use = "query execution starts only when the stream is pulled"]
pub struct AdbcQueryStream {
    connection: ManagedConnection,
    sql: String,
    parameters: Option<RecordBatch>,
    started: bool,
    execution: Option<QueryExecution>,
}

struct QueryExecution {
    // Fields are dropped in declaration order. The reader must release its
    // callbacks before the statement and its parent ADBC handles are released.
    reader: Box<dyn RecordBatchReader + Send + 'static>,
    _statement: ManagedStatement,
}

impl AdbcQueryStream {
    fn start(&mut self) -> Result<(), Error> {
        self.started = true;

        let mut statement = self.connection.new_statement()?;
        statement.set_sql_query(&self.sql)?;
        if let Some(parameters) = self.parameters.take() {
            statement.bind(parameters)?;
        }
        let reader = statement.execute()?;
        self.execution = Some(QueryExecution {
            reader,
            _statement: statement,
        });
        Ok(())
    }
}

impl Stream for AdbcQueryStream {
    type Item = Result<RecordBatch, Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if !this.started {
            if let Err(error) = this.start() {
                return Poll::Ready(Some(Err(error)));
            }
        }

        if this.execution.is_none() {
            return Poll::Ready(None);
        }

        let item = this
            .execution
            .as_mut()
            .and_then(|execution| execution.reader.next());
        match item {
            Some(Ok(batch)) => Poll::Ready(Some(Ok(batch))),
            Some(Err(error)) => {
                this.execution = None;
                Poll::Ready(Some(Err(error.into())))
            }
            None => {
                this.execution = None;
                Poll::Ready(None)
            }
        }
    }
}
