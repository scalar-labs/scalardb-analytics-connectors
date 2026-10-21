use connector_sdk::{
    ConnectorPlanner, NullOrdering, PlannedSubplan, PlanningAttempt, PlanningContext,
    PlanningError, PushdownDecision, PushdownResult, PushdownRule, ResidualPolicy,
    ValidatedCandidate, plan_pushdown,
};
use std::sync::OnceLock;

pub(crate) struct TestPlanner(pub(crate) PushdownDecision);

impl ConnectorPlanner for TestPlanner {
    fn plan_read(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(self.0.clone())
    }

    fn push_projection(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(self.0.clone())
    }

    fn push_filter(
        &self,
        _candidate: &ValidatedCandidate,
        _context: &PlanningContext,
    ) -> Result<PushdownDecision, PlanningError> {
        Ok(self.0.clone())
    }
}

pub(crate) fn exact_plan(bytes: &[u8], context: PlanningContext) -> PlannedSubplan {
    let PushdownResult::Exact { plan } = plan_pushdown(
        &TestPlanner(PushdownDecision::Exact),
        PlanningAttempt {
            rule: PushdownRule::Read,
            candidate: bytes,
            residual_policy: ResidualPolicy::ExactOnly,
            context: &context,
        },
    )
    .unwrap() else {
        panic!("expected exact pushdown");
    };
    plan
}

#[allow(dead_code)]
pub(crate) fn attempt<'a>(
    rule: PushdownRule,
    candidate: &'a [u8],
    residual_policy: ResidualPolicy,
) -> PlanningAttempt<'a> {
    PlanningAttempt {
        rule,
        candidate,
        residual_policy,
        context: default_context(),
    }
}

#[allow(dead_code)]
pub(crate) fn default_context() -> &'static PlanningContext {
    static CONTEXT: OnceLock<PlanningContext> = OnceLock::new();
    CONTEXT.get_or_init(|| context("binary"))
}

pub(crate) fn context(collation: &str) -> PlanningContext {
    PlanningContext::new(
        "postgresql",
        "17.2",
        "cluster-a",
        Some(collation.to_owned()),
        Some("UTC".to_owned()),
        Some("half-even".to_owned()),
        NullOrdering::NullsLast,
        Some("snapshot-42".to_owned()),
        "role-policy-v3",
    )
}
