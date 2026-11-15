//! Reaching Salesforce: the runtime the client runs on, the client of each
//! opened Data Source, and source reads that the SDK pulls synchronously and
//! cancels from any thread.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use connector_salesforce_client::{Client, Error};
use connector_sdk::{
    Access, Component, ConnectionOptions, Connector, ConnectorError, ConnectorStream,
    DataSourceAccess, ErrorClass, ErrorKind, ExecutionCancel, ExecutionOutput, LibrarySettings,
};
use futures_core::Stream;
use futures_util::{StreamExt, future};
use tokio::{
    runtime::{Builder, Handle, Runtime},
    sync::watch,
};

use crate::{SalesforceConnector, error_handler, options};

/// How many worker threads drive the client's I/O and timers. Every pull
/// runs its future on the pulling thread, so the workers only serve the
/// drivers and the HTTP connections' background tasks.
const WORKER_THREADS: usize = 1;

/// Direct access through the Salesforce REST API client.
pub struct SalesforceAccess;

/// One opened Data Source, and each of its sessions: a client for the org and
/// the runtime its requests run on.
///
/// Clones share one HTTP client and one access token. A session holds no
/// connection of its own; the HTTP client opens connections as requests need
/// them.
#[derive(Clone)]
pub struct SalesforceSession {
    pub(crate) client: Client,
    pub(crate) runtime: Handle,
}

impl Access<SalesforceConnector> for SalesforceAccess {
    type Library = Runtime;
    type DataSource = SalesforceSession;
    type Session = SalesforceSession;
    type Diagnostic = Error;

    fn description() -> DataSourceAccess {
        DataSourceAccess::Direct
    }

    fn open_library(_settings: &LibrarySettings) -> Result<Runtime, ConnectorError> {
        Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .thread_name("salesforce-connector")
            .enable_all()
            .build()
            .map_err(|_| {
                ConnectorError::new(
                    ErrorKind::Resource,
                    true,
                    "the Salesforce connector cannot start its runtime",
                )
            })
    }

    /// Creates the client and obtains an access token, so that invalid
    /// credentials are reported here rather than by the first request.
    fn open_data_source(
        runtime: &Runtime,
        _connector: &str,
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<SalesforceSession, ConnectorError> {
        let config = options::client_config(&options, connection_timeout)?;
        drop(options);
        let client = Client::new(config).map_err(SalesforceConnector::classify)?;
        let authenticate = client.authenticate();
        let authenticated = match connection_timeout {
            Some(limit) => runtime
                .block_on(async { tokio::time::timeout(limit, authenticate).await })
                .map_err(|_| {
                    ConnectorError::new(
                        ErrorKind::Timeout,
                        true,
                        "authenticating to Salesforce exceeded the connection timeout",
                    )
                })?,
            None => runtime.block_on(authenticate),
        };
        authenticated.map_err(SalesforceConnector::classify)?;
        Ok(SalesforceSession {
            client,
            runtime: runtime.handle().clone(),
        })
    }

    fn open_session(data_source: &SalesforceSession) -> Result<SalesforceSession, ConnectorError> {
        Ok(data_source.clone())
    }

    fn component(_session: &SalesforceSession) -> Option<Component> {
        None
    }

    fn error(diagnostic: Error, class: Option<ErrorClass>) -> ConnectorError {
        error_handler::error(diagnostic, class)
    }
}

impl SalesforceSession {
    /// Starts one source read over `stream`, which sends nothing before its
    /// first poll.
    ///
    /// Each pull runs the stream's next step on the pulling thread until it
    /// yields or the read is stopped. Stopping drops the stream, which ends
    /// its HTTP request.
    pub(crate) fn source_read<T, S>(&self, stream: S) -> ExecutionOutput<T>
    where
        T: Send + 'static,
        S: Stream<Item = Result<T, ConnectorError>> + Send + 'static,
    {
        let (stop, stopped) = watch::channel(false);
        let read = BlockingRead {
            runtime: self.runtime.clone(),
            stopped,
            stream: Some(Box::pin(stream)),
        };
        ExecutionOutput::new(Box::pin(read), Arc::new(Stop(stop)))
    }

    /// Starts a source read that runs `items` on its first pull and then
    /// returns its items one by one.
    pub(crate) fn source_items<T, F>(&self, items: F) -> ExecutionOutput<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<Vec<T>, ConnectorError>> + Send + 'static,
    {
        let stream = futures_util::stream::once(items).flat_map(|items| match items {
            Ok(items) => futures_util::stream::iter(items.into_iter().map(Ok)).left_stream(),
            Err(error) => futures_util::stream::iter(vec![Err(error)]).right_stream(),
        });
        self.source_read(stream)
    }
}

/// A source read the SDK pulls synchronously.
struct BlockingRead<T> {
    runtime: Handle,
    /// Becomes true once the read is stopped.
    stopped: watch::Receiver<bool>,
    /// `None` once the read ended or was stopped.
    stream: Option<ConnectorStream<T>>,
}

impl<T> BlockingRead<T> {
    /// Drops the stream inside the runtime, where its HTTP request is
    /// released.
    fn end(&mut self) {
        let _runtime = self.runtime.enter();
        self.stream = None;
    }
}

impl<T> Stream for BlockingRead<T> {
    type Item = Result<T, ConnectorError>;

    /// Runs the next step to completion; it never returns `Pending`.
    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(stream) = this.stream.as_mut() else {
            return Poll::Ready(None);
        };
        let mut stopped = this.stopped.clone();
        let step = this.runtime.block_on(async {
            let stop = async {
                // A cancel that was dropped can no longer stop the read.
                if stopped.wait_for(|stopped| *stopped).await.is_err() {
                    future::pending::<()>().await;
                }
            };
            match future::select(stream.next(), Box::pin(stop)).await {
                future::Either::Left((item, _)) => Some(item),
                future::Either::Right(_) => None,
            }
        });
        Poll::Ready(match step {
            Some(Some(Ok(item))) => Some(Ok(item)),
            Some(Some(Err(error))) => {
                this.end();
                Some(Err(error))
            }
            Some(None) => {
                this.end();
                None
            }
            None => {
                this.end();
                Some(Err(ConnectorError::new(
                    ErrorKind::Cancellation,
                    false,
                    "the Salesforce request was stopped",
                )))
            }
        })
    }
}

/// Stops one [`BlockingRead`] from any thread.
struct Stop(watch::Sender<bool>);

impl ExecutionCancel for Stop {
    fn cancel(&self) -> Result<(), ConnectorError> {
        self.0.send_replace(true);
        Ok(())
    }
}
