//! Planning and read behavior specific to the PostgreSQL connector.
//!
//! The conformance suite (`tests/conformance`) checks values, pushdown, schema
//! drift at read time, errors, and interruptions. These tests check what it
//! does not reach: restoring this connector's remote execution from bytes,
//! and the schema check that planning itself performs.

mod support;

use arrow_array::{Int32Array, RecordBatch};
use connector_postgresql::PostgresConnector;
use connector_sdk::{
    Connector, ConnectorError, ErrorKind, PlannedPushdown, PlannedRead, PlanningError,
    PushdownRequest, PushdownResult, RemoteExecution, conformance::plan::*, execute_planned,
};
use connector_sql::SqlRead;
use substrait::proto::{Plan, expression::literal::LiteralType};

use support::{PostgresSession, collect_ok, server, session};

type Read = PlannedRead<SqlRead>;

const SCHEMA: &str = "read_test";

fn setup() {
    server().reset_schema(
        SCHEMA,
        r#"
CREATE TABLE labelled (id integer NOT NULL, label text);
INSERT INTO labelled VALUES (1, 'a'), (2, 'b');
CREATE TABLE removed (id integer NOT NULL);
"#,
    );
}

#[test]
fn reads_follow_the_postgresql_connector() {
    setup();
    remote_executions_run_in_another_session();
    planning_a_dropped_table_is_a_stale_registration();
}

fn plan(session: &PostgresSession, plan: Plan) -> Result<PushdownResult<Read>, PlanningError> {
    PostgresConnector::plan(session, PushdownRequest { offered_plan: plan })
}

fn planned(session: &PostgresSession, offered: Plan) -> Box<PlannedPushdown<Read>> {
    match plan(session, offered).expect("planning succeeds") {
        PushdownResult::Planned { pushdown } => pushdown,
        PushdownResult::Unsupported { reason } => panic!("planning was unsupported: {reason}"),
    }
}

fn execute(
    session: &PostgresSession,
    remote: &RemoteExecution<Read>,
) -> Result<Vec<RecordBatch>, ConnectorError> {
    let mut stream = execute_planned(&remote.output_schema, None, || {
        PostgresConnector::read(session, remote)
    })?;
    Ok(collect_ok(&mut stream))
}

/// A remote execution planned on one session reads on another after a
/// round trip through bytes, as one carried to another process does.
fn remote_executions_run_in_another_session() {
    let planning = session();
    let offered = ReadPlan::new(
        [SCHEMA, "labelled"],
        vec![("id", i32(false)), ("label", text(true))],
    )
    .filter(call(
        "equal",
        vec![column(1), literal(LiteralType::String("b".to_owned()))],
    ))
    .project(vec![(0, "id")])
    .build();
    let bytes = planned(&planning, offered).remote.to_bytes();
    drop(planning);

    let reading = session();
    let remote = RemoteExecution::<Read>::from_bytes(&bytes).unwrap();
    let batches = execute(&reading, &remote).unwrap();
    let ids: Vec<i32> = batches
        .iter()
        .flat_map(|batch| {
            batch
                .column(0)
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap()
                .values()
                .to_vec()
        })
        .collect();
    assert_eq!(ids, [2]);
}

/// Planning loads the table's attributes from the catalog, so a table dropped
/// since registration fails planning as `stale registration`.
fn planning_a_dropped_table_is_a_stale_registration() {
    let session = session();
    server().psql(&format!("DROP TABLE {SCHEMA}.removed"));
    let offered = ReadPlan::new([SCHEMA, "removed"], vec![("id", i32(false))]).build();
    assert!(matches!(
        plan(&session, offered),
        Err(PlanningError::Connector(error)) if error.kind() == ErrorKind::StaleRegistration
    ));
}
