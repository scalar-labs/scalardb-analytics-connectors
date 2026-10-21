mod support;

use analytics_types::AnalyticsType;
use connector_sdk::{
    PlanningError, PushdownDecision, PushdownResult, PushdownRule, RequestError, ResidualPolicy,
    plan_pushdown,
};
use prost::Message;
use substrait::proto::{FetchRel, Rel, rel};
use support::{
    planner::{TestPlanner, attempt, context, default_context, exact_plan},
    substrait_plan::{
        EQUAL, OR, candidate_plan, column, i64_literal, read_mut, read_ref, root_mut, scalar,
    },
};

#[test]
fn exact_projection_keeps_substrait_as_the_plan_ast() {
    let plan = candidate_plan();
    let bytes = plan.encode_to_vec();
    let result = plan_pushdown(
        &TestPlanner(PushdownDecision::Exact),
        attempt(PushdownRule::Projection, &bytes, ResidualPolicy::ExactOnly),
    )
    .unwrap();

    let PushdownResult::Exact { plan: planned } = result else {
        panic!("expected exact pushdown");
    };
    assert_eq!(planned.plan(), &plan);
    assert_eq!(planned.encode_plan(), bytes);
    assert_eq!(planned.source_bindings()[0].names(), ["catalog", "users"]);
    assert_eq!(planned.output_schema().fields().len(), 2);
    assert_eq!(planned.output_schema().fields()[0].name(), "user_id");
    assert_eq!(
        planned.output_schema().fields()[1].data_type(),
        &AnalyticsType::Uuid
    );

    let serialized = planned.serialize();
    assert_eq!(serialized.substrait_plan(), bytes);
    assert_eq!(
        connector_sdk::PlannedSubplan::restore(serialized, default_context()).unwrap(),
        planned
    );
}

#[test]
fn retained_filter_accepts_an_or_expression_without_normalizing_it() {
    let mut plan = candidate_plan();
    let read = read_mut(&mut plan);
    read.filter = None;
    read.best_effort_filter = Some(Box::new(scalar(
        OR,
        vec![
            scalar(EQUAL, vec![column(0), i64_literal(7)]),
            scalar(EQUAL, vec![column(0), i64_literal(9)]),
        ],
    )));
    let bytes = plan.encode_to_vec();

    let result = plan_pushdown(
        &TestPlanner(PushdownDecision::Retained),
        attempt(
            PushdownRule::Filter,
            &bytes,
            ResidualPolicy::MayRetainOperator,
        ),
    )
    .unwrap();
    let PushdownResult::Retained { plan: planned } = result else {
        panic!("expected retained pushdown");
    };
    assert!(read_ref(planned.plan()).best_effort_filter.is_some());

    assert_eq!(
        plan_pushdown(
            &TestPlanner(PushdownDecision::Retained),
            attempt(PushdownRule::Filter, &bytes, ResidualPolicy::ExactOnly),
        ),
        Err(PlanningError::ResidualRequired)
    );
    assert_eq!(
        plan_pushdown(
            &TestPlanner(PushdownDecision::Exact),
            attempt(
                PushdownRule::Filter,
                &bytes,
                ResidualPolicy::MayRetainOperator
            ),
        ),
        Err(PlanningError::BestEffortRequiresResidual)
    );
    assert!(matches!(
        plan_pushdown(
            &TestPlanner(PushdownDecision::Exact),
            attempt(PushdownRule::Projection, &bytes, ResidualPolicy::ExactOnly),
        ),
        Ok(PushdownResult::Exact { .. })
    ));
}

#[test]
fn future_operator_rules_default_to_unsupported() {
    let bytes = candidate_plan().encode_to_vec();
    let result = plan_pushdown(
        &TestPlanner(PushdownDecision::Exact),
        attempt(PushdownRule::TopN, &bytes, ResidualPolicy::ExactOnly),
    )
    .unwrap();
    assert!(matches!(result, PushdownResult::Unsupported { reason } if reason.contains("TopN")));
}

#[test]
fn fingerprint_binds_the_plan_to_the_planning_context() {
    let bytes = candidate_plan().encode_to_vec();
    let first = exact_plan(&bytes, context("binary"));
    let second = exact_plan(&bytes, context("en_US"));
    assert_ne!(
        first.validation_fingerprint(),
        second.validation_fingerprint()
    );
}

#[test]
fn distinguishes_invalid_substrait_from_a_valid_unimplemented_relation() {
    let invalid = plan_pushdown(
        &TestPlanner(PushdownDecision::Exact),
        attempt(PushdownRule::Read, &[0xff, 0xff], ResidualPolicy::ExactOnly),
    );
    assert!(matches!(
        invalid,
        Err(PlanningError::InvalidCandidate(
            RequestError::InvalidRequest { .. }
        ))
    ));

    let mut plan = candidate_plan();
    let original = root_mut(&mut plan).input.take().unwrap();
    root_mut(&mut plan).input = Some(Rel {
        rel_type: Some(rel::RelType::Fetch(Box::new(FetchRel {
            input: Some(Box::new(original)),
            ..Default::default()
        }))),
    });
    let bytes = plan.encode_to_vec();
    let unsupported = plan_pushdown(
        &TestPlanner(PushdownDecision::Exact),
        attempt(PushdownRule::Limit, &bytes, ResidualPolicy::ExactOnly),
    );
    assert!(matches!(
        unsupported,
        Err(PlanningError::InvalidCandidate(
            RequestError::Undeclared { .. }
        ))
    ));
}

#[test]
fn rejects_a_different_substrait_version_and_oversized_plan() {
    let mut wrong_version = candidate_plan();
    wrong_version.version.as_mut().unwrap().minor_number = 68;
    assert!(matches!(
        plan_pushdown(
            &TestPlanner(PushdownDecision::Exact),
            attempt(
                PushdownRule::Read,
                &wrong_version.encode_to_vec(),
                ResidualPolicy::ExactOnly
            ),
        ),
        Err(PlanningError::InvalidCandidate(
            RequestError::Undeclared { .. }
        ))
    ));

    let mut oversized = candidate_plan();
    oversized.version.as_mut().unwrap().producer = "x".repeat(1024 * 1024);
    assert!(matches!(
        plan_pushdown(
            &TestPlanner(PushdownDecision::Exact),
            attempt(
                PushdownRule::Read,
                &oversized.encode_to_vec(),
                ResidualPolicy::ExactOnly
            ),
        ),
        Err(PlanningError::InvalidCandidate(
            RequestError::Undeclared { .. }
        ))
    ));
}

#[test]
fn official_validation_rejects_an_out_of_range_field_reference() {
    let mut plan = candidate_plan();
    read_mut(&mut plan).filter = Some(Box::new(scalar(EQUAL, vec![column(99), i64_literal(7)])));
    assert!(matches!(
        plan_pushdown(
            &TestPlanner(PushdownDecision::Exact),
            attempt(
                PushdownRule::Filter,
                &plan.encode_to_vec(),
                ResidualPolicy::ExactOnly
            ),
        ),
        Err(PlanningError::InvalidCandidate(
            RequestError::InvalidRequest { .. }
        ))
    ));
}
