//! A validated pushdown proposal ready for adoption and execution.

use ::substrait::proto::Plan;
use analytics_types::AnalyticsSchema;

use super::{LogicalPlan, LogicalRelation, PlanningError};
use crate::{RequestError, TableBinding};

/// Connector-owned, serializable instructions for executing one remote query.
///
/// This type intentionally does not implement `Debug`: its opaque payload may
/// contain SQL text, literal values, credentials, or other Connector-private
/// execution data.
#[derive(Clone, PartialEq, Eq)]
pub struct ExecutableQuery {
    /// Connector-defined payload format identifier.
    pub format: String,
    /// Connector-defined bytes dispatched during remote execution.
    pub payload: Vec<u8>,
}

impl ExecutableQuery {
    /// Creates executable instructions with a non-empty format and payload.
    ///
    /// The SDK treats both values as opaque Connector-owned data; it validates
    /// only that execution can identify the format and has bytes to dispatch.
    pub fn new(format: impl Into<String>, payload: Vec<u8>) -> Result<Self, RequestError> {
        let format = format.into();
        if format.is_empty() {
            return Err(RequestError::invalid(
                "an executable query format must be non-empty",
            ));
        }
        if payload.is_empty() {
            return Err(RequestError::invalid(
                "an executable query payload must be non-empty",
            ));
        }
        Ok(Self { format, payload })
    }
}

/// A proposed semantic decomposition into remote work followed by optional local work.
///
/// The Query Engine adapter decides whether it can lower and atomically adopt
/// this proposal.
///
/// This type intentionally does not implement `Debug`: it contains raw
/// Substrait boundary plans and an opaque [`ExecutableQuery`]. Inspect the
/// validated `LogicalPlan` before boundary conversion when diagnostics need a
/// redacted plan representation.
#[derive(Clone, PartialEq)]
pub struct PlannedPushdown {
    /// Complete Substrait plan implemented by the executable remote query.
    pub remote_plan: Plan,
    /// Remaining Substrait work for the Query Engine, when required.
    pub local_plan: Option<Plan>,
    /// Connector-owned instructions for executing `remote_plan`.
    pub executable_query: ExecutableQuery,
    /// Schema of the remote plan's terminal table read.
    pub remote_base_schema: AnalyticsSchema,
    /// Schema produced by remote execution and consumed by `local_plan`.
    pub remote_output_schema: AnalyticsSchema,
    /// Opaque table identifiers referenced by the remote plan.
    pub table_bindings: Vec<TableBinding>,
    /// Final schema produced by the complete remote/local decomposition.
    pub output_schema: AnalyticsSchema,
}

impl PlannedPushdown {
    /// Asserts the decomposition contract and constructs the boundary result.
    ///
    /// The internal remote and optional local plans are checked before they are
    /// converted to Substrait. The executable query is retained unchanged for
    /// later execution.
    pub fn try_new(
        offered_plan: &LogicalPlan,
        remote_plan: LogicalPlan,
        local_plan: Option<LogicalPlan>,
        executable_query: ExecutableQuery,
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
            executable_query,
            remote_base_schema,
            remote_output_schema,
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
