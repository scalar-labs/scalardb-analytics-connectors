use connector_sdk::{
    ConnectorPlanner, ExecutableQuery, LogicalPlan, PlannedPushdown, PlanningError,
    PushdownRequest, PushdownResult,
};
use substrait::proto::Plan;

use super::substrait_plan::pushed_remote_logical;

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum TestDecision {
    Complete,
    Unsupported,
}

pub(crate) struct TestPlanner(pub(crate) TestDecision);

impl ConnectorPlanner for TestPlanner {
    fn plan(&self, offered_plan: &LogicalPlan) -> Result<PushdownResult, PlanningError> {
        connector_result(self.0, offered_plan)
    }
}

fn connector_result(
    decision: TestDecision,
    offered_plan: &LogicalPlan,
) -> Result<PushdownResult, PlanningError> {
    if matches!(decision, TestDecision::Unsupported) {
        return Ok(PushdownResult::Unsupported {
            reason: "the complete region is unsupported".to_owned(),
        });
    }
    let pushdown = PlannedPushdown::try_new(
        offered_plan,
        pushed_remote_logical(offered_plan),
        None,
        ExecutableQuery::new(
            "test-query-v1",
            b"SELECT user_id, external_id FROM users".to_vec(),
        )?,
    )?;
    Ok(PushdownResult::Planned {
        pushdown: Box::new(pushdown),
    })
}

#[allow(dead_code)]
pub(crate) fn complete_pushdown(plan: Plan) -> PlannedPushdown {
    let PushdownResult::Planned { pushdown } = (PushdownRequest { offered_plan: plan })
        .plan(&TestPlanner(TestDecision::Complete))
        .unwrap()
    else {
        panic!("expected planned pushdown");
    };
    *pushdown
}

#[allow(dead_code)]
pub(crate) fn request(substrait_plan: Plan) -> PushdownRequest {
    PushdownRequest {
        offered_plan: substrait_plan,
    }
}
