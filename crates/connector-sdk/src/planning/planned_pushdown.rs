//! A validated pushdown proposal ready for adoption and execution.

use ::substrait::proto::Plan;
use analytics_types::{AnalyticsSchema, arrow_schema::Schema};

use super::{LogicalPlan, LogicalRelation, PlanningError};
use crate::{ConnectorError, ErrorKind, TableBinding};

/// A Connector-owned query that can cross a process boundary.
///
/// The bytes are opaque and valid only for the same library version that
/// produced them; no other component reads or writes them.
pub trait PortableQuery: Sized {
    /// Encodes the query for transport to another process.
    fn to_bytes(&self) -> Vec<u8>;

    /// Restores a query encoded by [`PortableQuery::to_bytes`].
    fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError>;
}

/// The part of a planned pushdown that remote execution needs.
///
/// Execution in the planning process and in another process starts from the
/// same value. Its bytes, produced by [`RemoteExecution::to_bytes`], carry no
/// credential, connection, or process-local pointer.
///
/// This type intentionally does not implement `Debug`: the query may contain
/// SQL text, literal values, or other Connector-private execution data.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteExecution<Q> {
    /// Connector-defined instructions dispatched during remote execution.
    pub query: Q,
    /// Schema produced by remote execution; every batch is checked against it.
    pub output_schema: AnalyticsSchema,
}

impl<Q: PortableQuery> RemoteExecution<Q> {
    /// Encodes the remote execution as opaque bytes for another process.
    ///
    /// The layout is the output schema's length as a little-endian `u64`, the
    /// schema as Arrow JSON, and then the Connector's query bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let schema = serde_json::to_vec(&self.output_schema.to_arrow())
            .expect("an Arrow schema always serializes to JSON");
        let query = self.query.to_bytes();
        let mut bytes = Vec::with_capacity(8 + schema.len() + query.len());
        bytes.extend_from_slice(&(schema.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&schema);
        bytes.extend_from_slice(&query);
        bytes
    }

    /// Restores a remote execution encoded by [`RemoteExecution::to_bytes`].
    ///
    /// Bytes that cannot be restored are an `invalid request`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError> {
        let (length, rest) = bytes
            .split_first_chunk::<8>()
            .ok_or_else(|| invalid_transport("the remote execution bytes are truncated"))?;
        let schema_length = usize::try_from(u64::from_le_bytes(*length))
            .ok()
            .filter(|schema_length| *schema_length <= rest.len())
            .ok_or_else(|| invalid_transport("the remote execution bytes are truncated"))?;
        let (schema, query) = rest.split_at(schema_length);
        let schema: Schema = serde_json::from_slice(schema)
            .map_err(|_| invalid_transport("the remote output schema cannot be decoded"))?;
        let output_schema = AnalyticsSchema::try_from_arrow(&schema).map_err(|_| {
            invalid_transport("the remote output schema is not an Analytics schema")
        })?;
        Ok(Self {
            query: Q::from_bytes(query)?,
            output_schema,
        })
    }
}

fn invalid_transport(message: &'static str) -> ConnectorError {
    ConnectorError::new(ErrorKind::InvalidRequest, false, message)
}

/// A proposed semantic decomposition into remote work followed by optional local work.
///
/// The Query Engine adapter decides whether it can lower and atomically adopt
/// this proposal.
///
/// This type intentionally does not implement `Debug`: it contains raw
/// Substrait boundary plans and a Connector-owned [`RemoteExecution`]. Inspect the
/// validated `LogicalPlan` before boundary conversion when diagnostics need a
/// redacted plan representation.
#[derive(Clone, PartialEq)]
pub struct PlannedPushdown<Q> {
    /// Complete Substrait plan implemented by the remote execution.
    pub remote_plan: Plan,
    /// Remaining Substrait work for the Query Engine, when required.
    pub local_plan: Option<Plan>,
    /// What execution needs to run `remote_plan`; its output feeds `local_plan`.
    pub remote: RemoteExecution<Q>,
    /// Schema of the remote plan's terminal table read.
    pub remote_base_schema: AnalyticsSchema,
    /// Opaque table identifiers referenced by the remote plan.
    pub table_bindings: Vec<TableBinding>,
    /// Final schema produced by the complete remote/local decomposition.
    pub output_schema: AnalyticsSchema,
}

impl<Q> PlannedPushdown<Q> {
    /// Asserts the decomposition contract and constructs the boundary result.
    ///
    /// The internal remote and optional local plans are checked before they are
    /// converted to Substrait. The query is retained unchanged, together with
    /// the remote output schema, for later execution.
    pub fn try_new(
        offered_plan: &LogicalPlan,
        remote_plan: LogicalPlan,
        local_plan: Option<LogicalPlan>,
        query: Q,
    ) -> Result<Self, PlanningError> {
        let remote_output_schema =
            Self::assert_decomposition_contract(offered_plan, &remote_plan, local_plan.as_ref());
        let output_schema = offered_plan.output_schema().clone();
        let LogicalRelation::Read {
            base_schema,
            table_binding,
            ..
        } = remote_plan.terminal_relation()
        else {
            unreachable!("the decomposition contract requires a remote table read");
        };
        let remote_base_schema = base_schema.clone();
        let table_bindings = vec![table_binding.clone()];
        let remote_plan = remote_plan.into();
        let local_plan = local_plan.map(Into::into);
        Ok(Self {
            remote_plan,
            local_plan,
            remote: RemoteExecution {
                query,
                output_schema: remote_output_schema,
            },
            remote_base_schema,
            table_bindings,
            output_schema,
        })
    }

    /// Asserts the internal contract of a Connector-produced query decomposition.
    ///
    /// The remote plan must terminate in one table read. When a local plan is
    /// present, it must terminate in one `RemoteResult`, whose schema must equal
    /// the remote plan's output schema, and its final output schema must equal
    /// the offered plan's output schema. Without a local plan, the remote plan's
    /// output schema must equal the offered plan's output schema directly.
    ///
    /// These plans are produced from an already validated offered plan by
    /// Connector rewrite rules. A violation is therefore a Connector
    /// implementation bug and causes an assertion failure; it is not reported
    /// as invalid external input.
    fn assert_decomposition_contract(
        offered_plan: &LogicalPlan,
        remote_plan: &LogicalPlan,
        local_plan: Option<&LogicalPlan>,
    ) -> AnalyticsSchema {
        let offered_output_schema = offered_plan.output_schema();
        let remote_output_schema = remote_plan.output_schema().clone();
        assert!(
            matches!(
                remote_plan.terminal_relation(),
                LogicalRelation::Read { .. }
            ),
            "a Connector-produced remote plan must terminate in a table read"
        );

        if let Some(local_plan) = local_plan {
            let local_output_schema = local_plan.output_schema();
            let LogicalRelation::RemoteResult {
                output_schema: remote_result_schema,
                ..
            } = local_plan.terminal_relation()
            else {
                panic!("a Connector-produced local plan must terminate in RemoteResult");
            };
            assert_eq!(
                remote_result_schema, &remote_output_schema,
                "the local RemoteResult schema must equal the remote plan output schema"
            );
            assert_eq!(
                local_output_schema, offered_output_schema,
                "the local plan output schema must equal the offered plan output schema"
            );
        } else {
            assert_eq!(
                &remote_output_schema, offered_output_schema,
                "the remote plan output schema must equal the offered plan output schema when no local plan is present"
            );
        }

        remote_output_schema
    }
}
