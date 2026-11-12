//! The planning and read flows the SDK runs for every Connector.

use std::{
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
};

use analytics_types::{AnalyticsSchema, arrow_schema::Schema};
use arrow_array::RecordBatch;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::{
    ConnectorError, ConnectorStream, ErrorKind, ExecutableQueryResult, ExecutionCancel,
    ExecutionOutput, LogicalPlan, LogicalRelation, PlannedPushdown, PlanningError, PortableQuery,
    PushdownRequest, PushdownResult, QueryPlan, RemoteExecution, RewriteContext,
    SourceTableMetadata, TableRecord, TableReference, check_schema, convert::convert_batch,
};

use super::{Catalog, Connector, Executor, Planner, Session};

/// The query of a planned read: the Connector's query and what the schema
/// check needs.
///
/// It intentionally does not implement `Debug`: the query may contain SQL
/// text or literal values.
#[derive(Clone, PartialEq)]
pub struct PlannedRead<Q> {
    /// The table read, as metadata requests name it.
    pub table: TableReference,
    /// The registered schema of every base column the plan reads, by the
    /// column names the table's metadata reports.
    pub base_schema: AnalyticsSchema,
    /// The Connector's query.
    pub query: Q,
}

/// The serialized head of a [`PlannedRead`]; the query's bytes follow it.
#[derive(Serialize, Deserialize)]
struct PlannedReadHead {
    table: Vec<String>,
    base_schema: Schema,
}

impl<Q: PortableQuery> PortableQuery for PlannedRead<Q> {
    /// The head's length as a little-endian `u64`, the head as JSON, and then
    /// the Connector's query bytes.
    fn to_bytes(&self) -> Vec<u8> {
        let head = serde_json::to_vec(&PlannedReadHead {
            table: self.table.names().to_vec(),
            base_schema: self.base_schema.to_arrow(),
        })
        .expect("a table reference and an Arrow schema serialize to JSON");
        let query = self.query.to_bytes();
        let mut bytes = Vec::with_capacity(8 + head.len() + query.len());
        bytes.extend_from_slice(&(head.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&head);
        bytes.extend_from_slice(&query);
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError> {
        let invalid = || {
            ConnectorError::new(
                ErrorKind::InvalidRequest,
                false,
                "the bytes are not a planned read",
            )
        };
        let (length, rest) = bytes.split_first_chunk::<8>().ok_or_else(invalid)?;
        let head_length = usize::try_from(u64::from_le_bytes(*length))
            .ok()
            .filter(|head_length| *head_length <= rest.len())
            .ok_or_else(invalid)?;
        let (head, query) = rest.split_at(head_length);
        let head: PlannedReadHead = serde_json::from_slice(head).map_err(|_| invalid())?;
        Ok(Self {
            table: TableReference::new(head.table).map_err(|_| invalid())?,
            base_schema: AnalyticsSchema::try_from_arrow(&head.base_schema)
                .map_err(|_| invalid())?,
            query: Q::from_bytes(query)?,
        })
    }
}

/// Plans one offered read: facts, rewrite rules, and query generation.
pub(super) fn plan<C: Connector>(
    session: &Session<C>,
    request: PushdownRequest,
) -> Result<PushdownResult<PlannedRead<C::Query>>, PlanningError> {
    let offered_plan = LogicalPlan::validate(request.offered_plan)?;
    let (table, facts) = C::Planner::planning_facts(session, &offered_plan)?;
    let query_plan = session.rules().rewrite(
        QueryPlan::initial(&offered_plan),
        RewriteContext::new(&offered_plan, &facts),
    )?;
    let query_plan = drop_pass_through_local_plan(query_plan);
    let query = match C::Planner::generate_query(query_plan.remote_plan(), &facts)? {
        ExecutableQueryResult::Executable(query) => query,
        ExecutableQueryResult::Unsupported { reason } => {
            return Ok(PushdownResult::Unsupported { reason });
        }
    };
    let Some((_, base_schema)) = query_plan.remote_plan().table_read() else {
        unreachable!("a remote plan always ends in its table read");
    };
    let read = PlannedRead {
        table,
        base_schema: base_schema.clone(),
        query,
    };
    let (remote_plan, local_plan) = query_plan.into_parts();
    let pushdown = PlannedPushdown::try_new(&offered_plan, remote_plan, local_plan, read)?;
    Ok(PushdownResult::Planned {
        pushdown: Box::new(pushdown),
    })
}

/// Drops a local plan that only returns the remote result.
fn drop_pass_through_local_plan(plan: QueryPlan) -> QueryPlan {
    let pass_through = plan
        .local_plan()
        .is_some_and(|local| matches!(local.relation(), LogicalRelation::RemoteResult { .. }));
    if !pass_through {
        return plan;
    }
    let (remote_plan, _) = plan.into_parts();
    QueryPlan::new(remote_plan, None)
}

/// Starts one planned read: the schema check, then the query.
///
/// Both reads are started here and send nothing before their first pull; the
/// query is pulled only after the check passes. The returned cancel stops the
/// running step and every later one, so a cancellation during the check also
/// keeps the query from being sent.
pub(super) fn read<C: Connector>(
    session: &Session<C>,
    remote: &RemoteExecution<PlannedRead<C::Query>>,
) -> Result<ExecutionOutput, ConnectorError> {
    let read = &remote.query;
    let (check, check_cancel) =
        C::Catalog::read_tables(session, std::slice::from_ref(&read.table))?.into_parts();
    let (data, data_cancel) = C::Executor::run(session, &read.query)?.into_parts();
    let steps = Arc::new(Steps {
        cancels: vec![check_cancel, data_cancel],
        current: Mutex::new(0),
    });
    let stream = PlannedReadStream::<C> {
        state: ReadState::Checking { check, data },
        base_schema: read.base_schema.clone(),
        output_schema: remote.output_schema.clone(),
        steps: Arc::clone(&steps),
    };
    Ok(ExecutionOutput::new(Box::pin(stream), steps))
}

type CatalogTable<C> = <<C as Connector>::Catalog as Catalog<C>>::Table;

enum ReadState<C: Connector> {
    /// Reading the table's current metadata; the query waits.
    Checking {
        check: ConnectorStream<TableRecord<CatalogTable<C>>>,
        data: ConnectorStream<RecordBatch>,
    },
    /// Reading the query's result.
    Reading(ConnectorStream<RecordBatch>),
    Finished,
}

struct PlannedReadStream<C: Connector> {
    state: ReadState<C>,
    base_schema: AnalyticsSchema,
    output_schema: AnalyticsSchema,
    steps: Arc<Steps>,
}

impl<C: Connector> Unpin for PlannedReadStream<C> {}

impl<C: Connector> Stream for PlannedReadStream<C> {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                ReadState::Checking { check, .. } => {
                    let current = match check.as_mut().poll_next(context) {
                        Poll::Ready(Some(Ok(TableRecord::Read { metadata, .. }))) => {
                            Some(metadata.to_table_metadata())
                        }
                        Poll::Ready(Some(Ok(TableRecord::NotFound { .. }))) | Poll::Ready(None) => {
                            None
                        }
                        Poll::Ready(Some(Ok(TableRecord::Unavailable { .. }))) => {
                            return this.fail(ConnectorError::new(
                                ErrorKind::Permission,
                                false,
                                "the registered table's metadata could not be read",
                            ));
                        }
                        Poll::Ready(Some(Err(error))) => return this.fail(error),
                        Poll::Pending => return Poll::Pending,
                    };
                    if let Err(error) = check_schema(&this.base_schema, current.as_ref()) {
                        return this.fail(error);
                    }
                    let ReadState::Checking { data, .. } =
                        std::mem::replace(&mut this.state, ReadState::Finished)
                    else {
                        unreachable!("the state was checked above");
                    };
                    this.steps.advance(QUERY);
                    this.state = ReadState::Reading(data);
                }
                ReadState::Reading(data) => {
                    return match data.as_mut().poll_next(context) {
                        Poll::Ready(Some(Ok(batch))) => {
                            match convert_batch(&batch, &this.output_schema, C::convert_column) {
                                Ok(batch) => Poll::Ready(Some(Ok(batch))),
                                Err(error) => this.fail(error),
                            }
                        }
                        Poll::Ready(Some(Err(error))) => this.fail(error),
                        Poll::Ready(None) => {
                            this.state = ReadState::Finished;
                            Poll::Ready(None)
                        }
                        Poll::Pending => Poll::Pending,
                    };
                }
                ReadState::Finished => return Poll::Ready(None),
            }
        }
    }
}

impl<C: Connector> PlannedReadStream<C> {
    /// Ends the read with `error`, releasing both steps.
    fn fail(&mut self, error: ConnectorError) -> Poll<Option<Result<RecordBatch, ConnectorError>>> {
        self.state = ReadState::Finished;
        Poll::Ready(Some(Err(error)))
    }
}

/// The index of the query in [`Steps::cancels`]; the schema check is first.
const QUERY: usize = 1;

/// The cancels of a read's steps, in order, and the step that is running.
///
/// The stream advances `current` as each step finishes; the caller cancels
/// from another thread. Both hold the lock, so a cancel never reaches a step
/// that already finished.
struct Steps {
    cancels: Vec<Arc<dyn ExecutionCancel>>,
    current: Mutex<usize>,
}

impl Steps {
    fn advance(&self, step: usize) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = step;
    }
}

impl ExecutionCancel for Steps {
    /// Stops the running step and every later one, so none of them sends
    /// anything afterwards.
    fn cancel(&self) -> Result<(), ConnectorError> {
        let current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        let mut result = Ok(());
        for step in &self.cancels[*current..] {
            if let Err(error) = step.cancel() {
                result = result.and(Err(error));
            }
        }
        result
    }
}
