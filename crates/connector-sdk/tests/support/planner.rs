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
    type Query = String;

    fn plan(&self, offered_plan: &LogicalPlan) -> Result<PushdownResult<String>, PlanningError> {
        connector_result(self.0, offered_plan)
    }
}

fn connector_result(
    decision: TestDecision,
    offered_plan: &LogicalPlan,
) -> Result<PushdownResult<String>, PlanningError> {
    if matches!(decision, TestDecision::Unsupported) {
        return Ok(PushdownResult::Unsupported {
            reason: "the complete region is unsupported".to_owned(),
        });
    }
    let pushdown = PlannedPushdown::try_new(
        offered_plan,
        pushed_remote_logical(offered_plan),
        None,
        ExecutableQuery {
            query: "SELECT user_id, external_id FROM users".to_owned(),
        },
    )?;
    Ok(PushdownResult::Planned {
        pushdown: Box::new(pushdown),
    })
}

#[allow(dead_code)]
pub(crate) fn complete_pushdown(plan: Plan) -> PlannedPushdown<String> {
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
