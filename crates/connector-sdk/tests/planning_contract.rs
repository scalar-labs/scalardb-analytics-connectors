mod support;

use analytics_types::AnalyticsType;
use connector_sdk::{
    ConnectorPlanner, ExecutableQuery, ExecutableQueryResult, LogicalPlan, LogicalRelation,
    PlannedPushdown, PlanningError, PushdownResult, QueryPlan, RequestError, RewriteContext,
    RewriteRule, RewriteRuleConnectorPlanner, RewriteRuleSet,
};
use std::sync::OnceLock;
use substrait::proto::{FetchRel, Rel, plan_rel, rel};
use support::{
    planner::{TestDecision, TestPlanner, request},
    substrait_plan::{
        EQUAL, column, i64_literal, pushed_remote, pushed_remote_logical, read_mut, read_ref,
        requested_plan, root_mut, scalar,
    },
};

#[test]
fn connector_returns_the_planned_pushdown_directly() {
    let plan = requested_plan();
    let result = request(plan.clone())
        .plan(&TestPlanner(TestDecision::Complete))
        .unwrap();

    let PushdownResult::Planned { pushdown } = result else {
        panic!("expected planned pushdown");
    };
    assert_eq!(pushdown.remote_plan, pushed_remote(&plan));
    assert!(pushdown.local_plan.is_none());
    assert_eq!(pushdown.executable_query.format, "test-query-v1");
    assert_eq!(
        pushdown.executable_query.payload,
        b"SELECT user_id, external_id FROM users"
    );
    assert_eq!(
        pushdown.table_bindings[0].components(),
        ["catalog", "users"]
    );
    assert_eq!(pushdown.output_schema.fields().len(), 2);
    assert_eq!(pushdown.output_schema.fields()[0].name(), "user_id");
    assert_eq!(
        pushdown.output_schema.fields()[1].data_type(),
        &AnalyticsType::Uuid
    );
}

#[test]
fn rewrite_rule_planner_preserves_an_unsupported_query_result() {
    let result = request(requested_plan())
        .plan(&UnsupportedQueryPlanner)
        .unwrap();
    assert_eq!(
        result,
        PushdownResult::Unsupported {
            reason: "remote semantics are not exact".to_owned(),
        }
    );
}

struct UnsupportedQueryPlanner;

impl RewriteRuleConnectorPlanner for UnsupportedQueryPlanner {
    type Metadata = ();

    fn load_metadata(&self, _offered_plan: &LogicalPlan) -> Result<Self::Metadata, PlanningError> {
        Ok(())
    }

    fn rewrite_rules(&self) -> &RewriteRuleSet<Self::Metadata> {
        static RULES: OnceLock<RewriteRuleSet<()>> = OnceLock::new();
        RULES.get_or_init(RewriteRuleSet::new)
    }

    fn create_executable_query(
        &self,
        _remote_plan: &LogicalPlan,
        _metadata: &Self::Metadata,
    ) -> Result<ExecutableQueryResult, PlanningError> {
        Ok(ExecutableQueryResult::Unsupported {
            reason: "remote semantics are not exact".to_owned(),
        })
    }
}

#[test]
fn rejects_best_effort_filters_in_offered_plans() {
    let mut offered = requested_plan();
    read_mut(&mut offered).best_effort_filter =
        Some(Box::new(scalar(EQUAL, vec![column(0), i64_literal(7)])));
    assert!(matches!(
        request(offered).plan(&TestPlanner(TestDecision::Complete)),
        Err(PlanningError::InvalidPlan(RequestError::Undeclared { .. }))
    ));
}

#[test]
fn rejects_scan_fields_and_reordered_operators_in_an_offered_plan() {
    let mut scan_filtered = requested_plan();
    read_mut(&mut scan_filtered).filter =
        Some(Box::new(scalar(EQUAL, vec![column(0), i64_literal(7)])));
    assert!(matches!(
        request(scan_filtered).plan(&TestPlanner(TestDecision::Complete)),
        Err(PlanningError::InvalidPlan(RequestError::Undeclared { .. }))
    ));

    let mut reordered = requested_plan();
    let project = match root_mut(&mut reordered)
        .input
        .take()
        .unwrap()
        .rel_type
        .unwrap()
    {
        rel::RelType::Project(project) => project,
        _ => unreachable!(),
    };
    let mut project = project;
    let filter = match project.input.take().unwrap().rel_type.unwrap() {
        rel::RelType::Filter(filter) => filter,
        _ => unreachable!(),
    };
    let mut filter = filter;
    project.input = filter.input.take();
    filter.input = Some(Box::new(Rel {
        rel_type: Some(rel::RelType::Project(project)),
    }));
    root_mut(&mut reordered).input = Some(Rel {
        rel_type: Some(rel::RelType::Filter(filter)),
    });
    assert!(matches!(
        request(reordered).plan(&TestPlanner(TestDecision::Complete)),
        Err(PlanningError::InvalidPlan(RequestError::Undeclared { .. }))
    ));
}

#[test]
fn rejects_project_metadata_outside_the_logical_plan_profile() {
    let mut plan = requested_plan();
    let project = match root_mut(&mut plan)
        .input
        .as_mut()
        .and_then(|relation| relation.rel_type.as_mut())
    {
        Some(rel::RelType::Project(project)) => project,
        _ => unreachable!(),
    };
    project
        .common
        .as_mut()
        .expect("the fixture project has common fields")
        .hint = Some(Default::default());

    assert!(matches!(
        request(plan).plan(&TestPlanner(TestDecision::Complete)),
        Err(PlanningError::InvalidPlan(RequestError::Undeclared { .. }))
    ));
}

#[test]
fn rejects_a_zero_column_project_at_the_validation_boundary() {
    let mut plan = requested_plan();
    root_mut(&mut plan).names.clear();
    let project = match root_mut(&mut plan)
        .input
        .as_mut()
        .and_then(|relation| relation.rel_type.as_mut())
    {
        Some(rel::RelType::Project(project)) => project,
        _ => unreachable!(),
    };
    project.expressions.clear();
    let Some(substrait::proto::rel_common::EmitKind::Emit(emit)) =
        project.common.as_mut().unwrap().emit_kind.as_mut()
    else {
        unreachable!()
    };
    emit.output_mapping.clear();

    assert!(matches!(
        request(plan).plan(&TestPlanner(TestDecision::Complete)),
        Err(PlanningError::InvalidPlan(RequestError::Undeclared { .. }))
    ));
}

#[test]
fn rejects_a_zero_column_read_at_the_validation_boundary() {
    let mut plan = requested_plan();
    let mut read = read_ref(&plan).clone();
    let schema = read.base_schema.as_mut().unwrap();
    schema.names.clear();
    schema.r#struct.as_mut().unwrap().types.clear();
    root_mut(&mut plan).names.clear();
    root_mut(&mut plan).input = Some(Rel {
        rel_type: Some(rel::RelType::Read(Box::new(read))),
    });

    assert!(matches!(
        request(plan).plan(&TestPlanner(TestDecision::Complete)),
        Err(PlanningError::InvalidPlan(RequestError::Undeclared { .. }))
    ));
}

#[test]
#[should_panic(
    expected = "the remote plan output schema must equal the offered plan output schema when no local plan is present"
)]
fn complete_pushdown_requires_remote_output_to_match_offered_output() {
    let offered = requested_plan();
    let _ = request(offered).plan(&MismatchedRemoteOutputPlanner);
}

struct MismatchedRemoteOutputPlanner;

impl ConnectorPlanner for MismatchedRemoteOutputPlanner {
    fn plan(&self, offered_plan: &LogicalPlan) -> Result<PushdownResult, PlanningError> {
        let mut remote = pushed_remote_logical(offered_plan);
        let LogicalRelation::Project { input, .. } = remote.relation().clone() else {
            unreachable!();
        };
        remote = remote.with_root(
            *input,
            vec!["id".to_owned(), "name".to_owned(), "external_id".to_owned()],
        );
        complete_result(offered_plan, remote, None, "SELECT * FROM users")
    }
}

#[test]
fn initial_query_plan_keeps_filter_and_projection_local() {
    let offered = requested_plan();
    let result = request(offered).plan(&InitialPlanPlanner).unwrap();
    let PushdownResult::Planned { pushdown } = result else {
        panic!("expected planned pushdown");
    };

    let remote_read = read_ref(&pushdown.remote_plan);
    assert!(remote_read.filter.is_none());
    assert!(remote_read.projection.is_none());
    assert_eq!(pushdown.remote_output_schema.fields().len(), 3);

    let local = pushdown.local_plan.as_ref().expect("local plan");
    assert_eq!(
        local.expected_type_urls,
        ["urn:scalardb:analytics:substrait:remote-result-schema:v1"]
    );
    let root = match &local.relations[0].rel_type {
        Some(plan_rel::RelType::Root(root)) => root,
        _ => panic!("expected local root"),
    };
    let project = match root
        .input
        .as_ref()
        .and_then(|input| input.rel_type.as_ref())
    {
        Some(rel::RelType::Project(project)) => project,
        _ => panic!("expected local project"),
    };
    let filter = match project
        .input
        .as_ref()
        .and_then(|input| input.rel_type.as_ref())
    {
        Some(rel::RelType::Filter(filter)) => filter,
        _ => panic!("expected local filter"),
    };
    assert!(matches!(
        filter
            .input
            .as_ref()
            .and_then(|input| input.rel_type.as_ref()),
        Some(rel::RelType::ExtensionLeaf(_))
    ));
}

struct InitialPlanPlanner;

impl RewriteRuleConnectorPlanner for InitialPlanPlanner {
    type Metadata = PlanningMetadata;

    fn load_metadata(&self, _offered_plan: &LogicalPlan) -> Result<Self::Metadata, PlanningError> {
        Ok(PlanningMetadata {
            query: "SELECT * FROM users",
            marker: "",
        })
    }

    fn rewrite_rules(&self) -> &RewriteRuleSet<Self::Metadata> {
        static RULES: OnceLock<RewriteRuleSet<PlanningMetadata>> = OnceLock::new();
        RULES.get_or_init(RewriteRuleSet::new)
    }

    fn create_executable_query(
        &self,
        _remote_plan: &LogicalPlan,
        metadata: &Self::Metadata,
    ) -> Result<ExecutableQueryResult, PlanningError> {
        Ok(ExecutableQuery::new("test-query-v1", metadata.query.as_bytes().to_vec())?.into())
    }
}

struct PlanningMetadata {
    query: &'static str,
    marker: &'static str,
}

#[test]
fn rewrite_rules_compose_and_no_op_does_not_stop_the_sequence() {
    let offered = requested_plan();
    let result = request(offered).plan(&ComposedRulePlanner).unwrap();
    let PushdownResult::Planned { pushdown } = result else {
        panic!("expected planned pushdown");
    };
    assert_eq!(
        pushdown.remote_plan.version.as_ref().unwrap().producer,
        "connector-sdk-tests:first:metadata:second"
    );
}

struct ComposedRulePlanner;

impl RewriteRuleConnectorPlanner for ComposedRulePlanner {
    type Metadata = PlanningMetadata;

    fn load_metadata(&self, _offered_plan: &LogicalPlan) -> Result<Self::Metadata, PlanningError> {
        Ok(PlanningMetadata {
            query: "SELECT composed FROM users",
            marker: ":metadata",
        })
    }

    fn rewrite_rules(&self) -> &RewriteRuleSet<Self::Metadata> {
        static RULES: OnceLock<RewriteRuleSet<PlanningMetadata>> = OnceLock::new();
        RULES.get_or_init(|| {
            let mut rules = RewriteRuleSet::new();
            rules.push(AppendMarkerRule(":first"));
            rules.push(NoOpRule);
            rules.push(AppendMetadataMarkerRule);
            rules.push(AppendMarkerRule(":second"));
            rules
        })
    }

    fn create_executable_query(
        &self,
        _remote_plan: &LogicalPlan,
        metadata: &Self::Metadata,
    ) -> Result<ExecutableQueryResult, PlanningError> {
        Ok(ExecutableQuery::new("test-query-v1", metadata.query.as_bytes().to_vec())?.into())
    }
}

struct AppendMarkerRule(&'static str);

impl RewriteRule<PlanningMetadata> for AppendMarkerRule {
    fn id(&self) -> &'static str {
        self.0
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        _context: RewriteContext<'_, PlanningMetadata>,
    ) -> Result<QueryPlan, PlanningError> {
        let (mut remote, local) = plan.into_parts();
        let mut version = remote
            .version()
            .cloned()
            .expect("the offered logical plan has a version");
        version.producer.push_str(self.0);
        remote = remote.with_version(Some(version));
        Ok(QueryPlan::new(remote, local))
    }
}

struct NoOpRule;

impl RewriteRule<PlanningMetadata> for NoOpRule {
    fn id(&self) -> &'static str {
        "no-op"
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        _context: RewriteContext<'_, PlanningMetadata>,
    ) -> Result<QueryPlan, PlanningError> {
        Ok(plan)
    }
}

struct AppendMetadataMarkerRule;

impl RewriteRule<PlanningMetadata> for AppendMetadataMarkerRule {
    fn id(&self) -> &'static str {
        "append-metadata-marker"
    }

    fn rewrite(
        &self,
        plan: QueryPlan,
        context: RewriteContext<'_, PlanningMetadata>,
    ) -> Result<QueryPlan, PlanningError> {
        let (mut remote, local) = plan.into_parts();
        let mut version = remote
            .version()
            .cloned()
            .expect("the offered logical plan has a version");
        version.producer.push_str(context.metadata.marker);
        remote = remote.with_version(Some(version));
        Ok(QueryPlan::new(remote, local))
    }
}

#[test]
fn rewrite_rule_error_stops_the_sequence() {
    let offered = requested_plan();
    assert!(matches!(
        request(offered).plan(&FailingRulePlanner),
        Err(PlanningError::Connector { reason }) if reason.contains("catalog lookup")
    ));
}

struct FailingRulePlanner;

impl RewriteRuleConnectorPlanner for FailingRulePlanner {
    type Metadata = PlanningMetadata;

    fn load_metadata(&self, _offered_plan: &LogicalPlan) -> Result<Self::Metadata, PlanningError> {
        Ok(PlanningMetadata {
            query: "SELECT unreachable FROM users",
            marker: "",
        })
    }

    fn rewrite_rules(&self) -> &RewriteRuleSet<Self::Metadata> {
        static RULES: OnceLock<RewriteRuleSet<PlanningMetadata>> = OnceLock::new();
        RULES.get_or_init(|| {
            let mut rules = RewriteRuleSet::new();
            rules.push(FailingRule);
            rules
        })
    }

    fn create_executable_query(
        &self,
        _remote_plan: &LogicalPlan,
        _metadata: &Self::Metadata,
    ) -> Result<ExecutableQueryResult, PlanningError> {
        unreachable!("the failing rule must stop the common planner flow")
    }
}

struct FailingRule;

impl RewriteRule<PlanningMetadata> for FailingRule {
    fn id(&self) -> &'static str {
        "failing"
    }

    fn rewrite(
        &self,
        _plan: QueryPlan,
        _context: RewriteContext<'_, PlanningMetadata>,
    ) -> Result<QueryPlan, PlanningError> {
        Err(PlanningError::Connector {
            reason: "catalog lookup failed".to_owned(),
        })
    }
}

#[test]
#[should_panic(expected = "a Connector-produced local plan must terminate in RemoteResult")]
fn local_plan_cannot_read_the_named_table_again() {
    let offered = requested_plan();
    let _ = request(offered).plan(&LocalReadPlanner);
}

struct LocalReadPlanner;

impl ConnectorPlanner for LocalReadPlanner {
    fn plan(&self, offered_plan: &LogicalPlan) -> Result<PushdownResult, PlanningError> {
        let local = offered_plan.clone();
        complete_result(
            offered_plan,
            pushed_remote_logical(offered_plan),
            Some(local),
            "SELECT projected FROM users",
        )
    }
}

#[test]
fn unsupported_request_produces_no_plans() {
    let plan = requested_plan();
    assert!(matches!(
        request(plan).plan(&TestPlanner(TestDecision::Unsupported)),
        Ok(PushdownResult::Unsupported { reason }) if reason.contains("complete region")
    ));
}

#[test]
fn rejects_an_unimplemented_offered_relation() {
    let mut plan = requested_plan();
    let original = root_mut(&mut plan).input.take().unwrap();
    root_mut(&mut plan).input = Some(Rel {
        rel_type: Some(rel::RelType::Fetch(Box::new(FetchRel {
            input: Some(Box::new(original)),
            ..Default::default()
        }))),
    });
    assert!(matches!(
        request(plan).plan(&TestPlanner(TestDecision::Complete)),
        Err(PlanningError::InvalidPlan(RequestError::Undeclared { .. }))
    ));
}

fn complete_result(
    offered_plan: &LogicalPlan,
    remote: LogicalPlan,
    local: Option<LogicalPlan>,
    query: &str,
) -> Result<PushdownResult, PlanningError> {
    let pushdown = PlannedPushdown::try_new(
        offered_plan,
        remote,
        local,
        ExecutableQuery::new("test-query-v1", query.as_bytes().to_vec())?,
    )?;
    Ok(PushdownResult::Planned {
        pushdown: Box::new(pushdown),
    })
}
