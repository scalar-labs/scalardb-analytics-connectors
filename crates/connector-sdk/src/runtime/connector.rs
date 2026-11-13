//! Object-safe views of a Connector's units.
//!
//! A [`Connector`]'s units carry types that differ per connector. The library
//! holds connectors of different types in one registry and dispatches by
//! Connector name at run time, so each unit is implemented once here for every
//! Connector and used only through `dyn`.

use std::{
    any::Any,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    Access, Catalog, Component, ConnectionOptions, Connector, ConnectorError, DataSource,
    ErrorKind, ExecutionOutput, ExecutionStream, LibrarySettings, Listed, NameFilter,
    NamespacePath, PlannedRead, PlanningError, PortableQuery, PushdownRequest, PushdownResult,
    RemoteExecution, RequestError, Session, SourceStream, SourceTableMetadata, TableEntry,
    TableRecord, TableReference, connector::ConnectorState, execute_planned, start_source_read,
};
use arrow_schema::SchemaRef;

use super::metadata::{MapItems, SourceMetadata, TableRead};
use prost::Message;
use substrait::proto::Plan;

/// A connector's process-level state behind one type.
pub(crate) trait DynConnectorLibrary: Send + Sync {
    fn open_data_source(
        &self,
        connector: &str,
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Box<dyn DynDataSource>, ConnectorError>;
}

impl<C: Connector> DynConnectorLibrary for ConnectorState<C> {
    fn open_data_source(
        &self,
        connector: &str,
        options: ConnectionOptions,
        connection_timeout: Option<Duration>,
    ) -> Result<Box<dyn DynDataSource>, ConnectorError> {
        let data_source =
            ConnectorState::open_data_source(self, connector, options, connection_timeout)?;
        Ok(Box::new(data_source))
    }
}

/// Creates one connector's process-level state from the library settings.
pub(crate) fn create_library<C: Connector>(
    settings: &LibrarySettings,
) -> Result<Box<dyn DynConnectorLibrary>, ConnectorError> {
    Ok(Box::new(ConnectorState::<C>::new(settings)?))
}

/// A connector's Data Source behind one type.
pub(crate) trait DynDataSource: Send + Sync {
    fn open_session(&self) -> Result<Box<dyn DynSession>, ConnectorError>;
}

impl<C: Connector> DynDataSource for DataSource<C> {
    fn open_session(&self) -> Result<Box<dyn DynSession>, ConnectorError> {
        Ok(Box::new(DataSource::open_session(self)?))
    }
}

/// The outcome of planning one pushdown request, with its plans encoded.
pub enum PlanningOutcome {
    Planned {
        /// The remote Substrait plan as protobuf bytes.
        remote_plan: Vec<u8>,
        /// The local Substrait plan as protobuf bytes, when local work remains.
        local_plan: Option<Vec<u8>>,
        remote: SharedRemoteExecution,
    },
    Unsupported {
        reason: String,
    },
}

/// A connector's session behind one type.
pub(crate) trait DynSession: Send + Sync {
    /// Validates the encoded Substrait plan and lets the Connector plan it.
    fn plan_pushdown(&self, offered_plan: &[u8]) -> Result<PlanningOutcome, ConnectorError>;

    /// Restores a remote execution encoded in another process.
    fn restore_remote_execution(
        &self,
        bytes: &[u8],
    ) -> Result<SharedRemoteExecution, ConnectorError>;

    /// Fails unless this session's connector can execute `remote`.
    fn check_remote_execution(&self, remote: &SharedRemoteExecution) -> Result<(), ConnectorError>;

    /// Starts the remote execution on this session's executor.
    fn execute(
        &self,
        remote: &SharedRemoteExecution,
        deadline: Option<Instant>,
    ) -> Result<ExecutionStream, ConnectorError>;

    /// Starts a namespace list.
    fn namespace_list(
        &self,
        namespaces: &NameFilter,
        deadline: Option<Instant>,
    ) -> Result<SourceStream<Listed<NamespacePath>>, ConnectorError>;

    /// Starts a table list.
    fn table_list(
        &self,
        namespaces: &NameFilter,
        tables: &NameFilter,
        deadline: Option<Instant>,
    ) -> Result<SourceStream<Listed<TableEntry>>, ConnectorError>;

    /// Starts a table metadata request.
    ///
    /// The connector's source metadata type is erased here: each read table
    /// becomes its derived table metadata and its JSON document, labelled
    /// with `connector`.
    fn table_metadata(
        &self,
        tables: &[TableReference],
        connector: &str,
        deadline: Option<Instant>,
    ) -> Result<SourceStream<TableRecord<TableRead>>, ConnectorError>;

    /// Returns the ADBC driver the session reads through; `None` with direct access.
    fn adbc_driver(&self) -> Option<Component>;
}

impl<C: Connector> DynSession for Session<C> {
    fn plan_pushdown(&self, offered_plan: &[u8]) -> Result<PlanningOutcome, ConnectorError> {
        let offered_plan = Plan::decode(offered_plan).map_err(|_| {
            ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "the offered plan is not a Substrait plan",
            )
        })?;
        let result = C::plan(self, PushdownRequest { offered_plan }).map_err(planning_error)?;
        Ok(match result {
            PushdownResult::Planned { pushdown } => PlanningOutcome::Planned {
                remote_plan: pushdown.remote_plan.encode_to_vec(),
                local_plan: pushdown.local_plan.as_ref().map(Message::encode_to_vec),
                remote: SharedRemoteExecution(Arc::new(pushdown.remote)),
            },
            PushdownResult::Unsupported { reason } => PlanningOutcome::Unsupported { reason },
        })
    }

    fn restore_remote_execution(
        &self,
        bytes: &[u8],
    ) -> Result<SharedRemoteExecution, ConnectorError> {
        let remote = RemoteExecution::<PlannedRead<C::Query>>::from_bytes(bytes)?;
        Ok(SharedRemoteExecution(Arc::new(remote)))
    }

    fn check_remote_execution(&self, remote: &SharedRemoteExecution) -> Result<(), ConnectorError> {
        typed_remote_execution::<PlannedRead<C::Query>>(remote).map(|_| ())
    }

    fn execute(
        &self,
        remote: &SharedRemoteExecution,
        deadline: Option<Instant>,
    ) -> Result<ExecutionStream, ConnectorError> {
        let remote = typed_remote_execution::<PlannedRead<C::Query>>(remote)?;
        execute_planned(&remote.output_schema, deadline, || C::read(self, remote))
    }

    fn namespace_list(
        &self,
        namespaces: &NameFilter,
        deadline: Option<Instant>,
    ) -> Result<SourceStream<Listed<NamespacePath>>, ConnectorError> {
        start_source_read(deadline, || C::Catalog::namespace_list(self, namespaces))
    }

    fn table_list(
        &self,
        namespaces: &NameFilter,
        tables: &NameFilter,
        deadline: Option<Instant>,
    ) -> Result<SourceStream<Listed<TableEntry>>, ConnectorError> {
        start_source_read(deadline, || {
            C::Catalog::table_list(self, namespaces, tables)
        })
    }

    fn table_metadata(
        &self,
        tables: &[TableReference],
        connector: &str,
        deadline: Option<Instant>,
    ) -> Result<SourceStream<TableRecord<TableRead>>, ConnectorError> {
        let connector = connector.to_owned();
        start_source_read(deadline, || {
            let (records, cancel) = C::Catalog::table_metadata(self, tables)?.into_parts();
            let records = MapItems::new(records, move |record| erase(record, &connector));
            Ok(ExecutionOutput::new(Box::pin(records), cancel))
        })
    }

    fn adbc_driver(&self) -> Option<Component> {
        C::Access::component(self.access())
    }
}

/// Erases a record's source metadata type into its derived table metadata
/// and its JSON document.
fn erase<T: SourceTableMetadata>(
    record: TableRecord<T>,
    connector: &str,
) -> Result<TableRecord<TableRead>, ConnectorError> {
    Ok(match record {
        TableRecord::Read { metadata, read_at } => {
            let document = serde_json::to_string(&metadata).map_err(|_| {
                ConnectorError::new(
                    ErrorKind::Internal,
                    false,
                    "the source metadata could not be serialized as JSON",
                )
            })?;
            TableRecord::Read {
                metadata: TableRead {
                    table_metadata: metadata.to_table_metadata(),
                    source_metadata: SourceMetadata {
                        connector: connector.to_owned(),
                        document,
                    },
                },
                read_at,
            }
        }
        TableRecord::NotFound { table } => TableRecord::NotFound { table },
        TableRecord::Unavailable { table, reason } => TableRecord::Unavailable { table, reason },
    })
}

/// Converts a planning failure into the error returned by `plan_pushdown`.
fn planning_error(error: PlanningError) -> ConnectorError {
    match error {
        PlanningError::InvalidPlan(
            RequestError::InvalidRequest { reason } | RequestError::Undeclared { reason },
        ) => ConnectorError::new(ErrorKind::InvalidRequest, false, reason),
        PlanningError::Connector(error) => error,
    }
}

/// Recovers a shared remote execution with the session connector's query type.
fn typed_remote_execution<Q: 'static>(
    remote: &SharedRemoteExecution,
) -> Result<&RemoteExecution<Q>, ConnectorError> {
    remote
        .0
        .as_any()
        .downcast_ref::<RemoteExecution<Q>>()
        .ok_or_else(|| {
            ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "the remote execution was planned by a different connector",
            )
        })
}

/// A remote execution whose query type is known only to its connector.
#[derive(Clone)]
pub struct SharedRemoteExecution(Arc<dyn PortableRemoteExecution>);

impl SharedRemoteExecution {
    /// Encodes the remote execution as opaque bytes for another process.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.to_bytes()
    }

    /// Returns the Arrow schema every batch of the remote execution has.
    pub fn output_schema(&self) -> SchemaRef {
        self.0.output_schema()
    }
}

/// The operations the library needs from a `RemoteExecution<Q>` of any `Q`.
trait PortableRemoteExecution: Send + Sync {
    fn to_bytes(&self) -> Vec<u8>;
    fn output_schema(&self) -> SchemaRef;
    fn as_any(&self) -> &dyn Any;
}

impl<Q> PortableRemoteExecution for RemoteExecution<Q>
where
    Q: PortableQuery + Send + Sync + 'static,
{
    fn to_bytes(&self) -> Vec<u8> {
        RemoteExecution::to_bytes(self)
    }

    fn output_schema(&self) -> SchemaRef {
        Arc::new(self.output_schema.to_arrow())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
