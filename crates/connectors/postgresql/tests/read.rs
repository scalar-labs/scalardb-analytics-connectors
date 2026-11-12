//! Pushdown planning and data read against a PostgreSQL server.

mod support;

use std::{
    thread,
    time::{Duration, Instant},
};

use arrow_array::{
    Array, BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float64Array, Int16Array,
    Int32Array, Int64Array, IntervalMonthDayNanoArray, RecordBatch, StringArray,
    Time64MicrosecondArray, TimestampMicrosecondArray, types::IntervalMonthDayNano,
};
use connector_postgresql::PostgresConnector;
use connector_sdk::{
    Connector, ConnectorError, DiagnosticValue, ErrorKind, PlannedPushdown, PlannedRead,
    PlanningError, PushdownRequest, PushdownResult, RemoteExecution, execute_planned,
};
use connector_sql::SqlRead;
use substrait::proto::{Expression, Plan, expression::literal::LiteralType};

use support::{
    PASSWORD, PostgresSession, collect, collect_ok, library, open, open_session, options, plan::*,
    server, session,
};

type Read = PlannedRead<SqlRead>;

const SCHEMA: &str = "read_test";

fn setup() {
    server().reset_schema(
        SCHEMA,
        r#"
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
CREATE COLLATION case_insensitive (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
CREATE TABLE value_types (
  id integer NOT NULL,
  flag boolean,
  small smallint,
  big bigint,
  ratio double precision,
  exact numeric(38, 4),
  free_text text,
  padded char(5),
  bytes bytea,
  day date,
  clock time,
  stamp timestamp,
  instant timestamptz,
  span interval,
  external_id uuid,
  document json,
  binary_document jsonb,
  feeling mood,
  zoned timetz
);
INSERT INTO value_types VALUES
  (1, true, 32767, -9223372036854775808, 0.25,
   '9999999999999999999999999999999999.9999', 'héllo', 'ab', '\x00ff',
   '0001-01-01 BC', '23:59:59.999999', '1970-01-01 00:00:00.000001',
   '2020-01-01 00:00:00+09', '1 year 2 mons 3 days 04:05:06.789',
   'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', '{"a": 1}', '{"b": [1, 2]}', 'ok',
   '10:00:00.25+05:30'),
  (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
   NULL, NULL, NULL, NULL, NULL, NULL);
CREATE TABLE special (id integer NOT NULL, amount numeric(10, 2), clock time);
INSERT INTO special VALUES (1, 'NaN', '12:00'), (2, 1.50, '24:00:00'), (3, -0.01, '00:00');
CREATE TABLE filtering (
  id integer NOT NULL,
  label text,
  code char(3),
  folded text COLLATE case_insensitive,
  score double precision,
  external_id uuid,
  c_label text COLLATE "C",
  posix_label text COLLATE "POSIX",
  day date
);
INSERT INTO filtering VALUES
  (1, 'a', 'a', 'A', 0.5, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', 'a', 'a', '2020-01-01'),
  (2, 'b', 'b', 'b', 'NaN', NULL, 'b', 'c', NULL),
  (3, 'a ', 'a', 'a', 1.5, NULL, 'a', 'b', '2021-01-01');
CREATE TABLE altered (id integer NOT NULL, value integer NOT NULL);
CREATE FUNCTION slow_value() RETURNS integer LANGUAGE sql AS 'SELECT 1 FROM pg_sleep(30)';
CREATE VIEW slow AS SELECT slow_value() AS n;
"#,
    );
}

#[test]
fn reads_follow_the_planned_decomposition() {
    setup();
    every_covered_type_reads_its_exact_values();
    values_that_cannot_be_carried_are_data_errors();
    filters_run_remotely_only_with_the_offered_meaning();
    remote_executions_run_in_another_session();
    schema_changes_are_stale_registrations();
    cancellation_and_deadlines_stop_the_query_at_the_source();
    errors_reveal_neither_statements_nor_secrets();
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
) -> (Vec<RecordBatch>, Option<ConnectorError>) {
    let mut stream = execute_planned(&remote.output_schema, None, || {
        PostgresConnector::read(session, remote)
    })
    .expect("execution starts");
    collect(&mut stream)
}

fn execute_ok(session: &PostgresSession, remote: &RemoteExecution<Read>) -> RecordBatch {
    let mut stream = execute_planned(&remote.output_schema, None, || {
        PostgresConnector::read(session, remote)
    })
    .expect("execution starts");
    let batches = collect_ok(&mut stream);
    arrow_select_concat(&batches)
}

fn arrow_select_concat(batches: &[RecordBatch]) -> RecordBatch {
    assert_eq!(batches.len(), 1, "the test tables fit in one batch");
    batches[0].clone()
}

fn values<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> &'a T {
    batch
        .column_by_name(name)
        .unwrap_or_else(|| panic!("no column {name}"))
        .as_any()
        .downcast_ref::<T>()
        .unwrap_or_else(|| panic!("column {name} has another type"))
}

fn ids(batch: &RecordBatch) -> Vec<i32> {
    let mut ids: Vec<i32> = values::<Int32Array>(batch, "id").iter().flatten().collect();
    ids.sort_unstable();
    ids
}

fn value_types_plan() -> ReadPlan {
    ReadPlan::new(
        [SCHEMA, "value_types"],
        vec![
            ("id", i32(false)),
            ("flag", boolean(true)),
            ("small", i16(true)),
            ("big", i64(true)),
            ("ratio", fp64(true)),
            ("exact", decimal(38, 4, true)),
            ("free_text", text(true)),
            ("padded", text(true)),
            ("bytes", binary(true)),
            ("day", date(true)),
            ("clock", time(true)),
            ("stamp", timestamp(true)),
            ("instant", timestamp_tz(true)),
            ("span", interval(true)),
            ("external_id", logical("uuid", "{}", true)),
            ("document", logical("json", "{}", true)),
            ("binary_document", logical("json", "{}", true)),
            (
                "feeling",
                logical("enum", r#"{"members":["sad","ok","happy"]}"#, true),
            ),
            ("zoned", logical("timetz", "{}", true)),
        ],
    )
}

fn every_covered_type_reads_its_exact_values() {
    let session = session();
    let pushdown = planned(&session, value_types_plan().build());
    assert!(pushdown.local_plan.is_none());
    let batch = execute_ok(&session, &pushdown.remote);
    assert_eq!(
        batch.schema().as_ref(),
        &pushdown.remote.output_schema.to_arrow()
    );
    assert_eq!(values::<Int32Array>(&batch, "id").values(), &[1, 2]);

    assert!(values::<BooleanArray>(&batch, "flag").value(0));
    assert_eq!(values::<Int16Array>(&batch, "small").value(0), i16::MAX);
    assert_eq!(values::<Int64Array>(&batch, "big").value(0), i64::MIN);
    assert_eq!(values::<Float64Array>(&batch, "ratio").value(0), 0.25);
    let exact = values::<Decimal128Array>(&batch, "exact");
    assert_eq!(
        exact.value(0),
        99_999_999_999_999_999_999_999_999_999_999_999_999
    );
    assert_eq!((exact.precision(), exact.scale()), (38, 4));
    assert_eq!(values::<StringArray>(&batch, "free_text").value(0), "héllo");
    assert_eq!(values::<StringArray>(&batch, "padded").value(0), "ab   ");
    assert_eq!(
        values::<BinaryArray>(&batch, "bytes").value(0),
        [0x00, 0xff]
    );
    // 1 BC is year 0 of the proleptic Gregorian calendar.
    assert_eq!(values::<Date32Array>(&batch, "day").value(0), -719_528);
    assert_eq!(
        values::<Time64MicrosecondArray>(&batch, "clock").value(0),
        86_399_999_999
    );
    assert_eq!(
        values::<TimestampMicrosecondArray>(&batch, "stamp").value(0),
        1
    );
    assert_eq!(
        values::<TimestampMicrosecondArray>(&batch, "instant").value(0),
        1_577_804_400_000_000
    );
    assert_eq!(
        values::<IntervalMonthDayNanoArray>(&batch, "span").value(0),
        IntervalMonthDayNano::new(14, 3, 14_706_789_000_000)
    );
    assert_eq!(
        values::<StringArray>(&batch, "external_id").value(0),
        "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"
    );
    assert_eq!(
        values::<StringArray>(&batch, "document").value(0),
        r#"{"a": 1}"#
    );
    assert_eq!(
        values::<StringArray>(&batch, "binary_document").value(0),
        r#"{"b": [1, 2]}"#
    );
    assert_eq!(values::<StringArray>(&batch, "feeling").value(0), "ok");
    assert_eq!(
        values::<StringArray>(&batch, "zoned").value(0),
        "10:00:00.25+05:30"
    );

    for field in batch.schema().fields().iter().skip(1) {
        assert!(
            batch.column_by_name(field.name()).unwrap().is_null(1),
            "{}",
            field.name()
        );
    }
}

fn special_plan() -> ReadPlan {
    ReadPlan::new(
        [SCHEMA, "special"],
        vec![
            ("id", i32(false)),
            ("amount", decimal(10, 2, true)),
            ("clock", time(true)),
        ],
    )
}

fn values_that_cannot_be_carried_are_data_errors() {
    let session = session();
    for (column_position, name) in [(1, "amount"), (2, "clock")] {
        let offered = special_plan()
            .project(vec![(0, "id"), (column_position, name)])
            .build();
        let pushdown = planned(&session, offered);
        let (_, error) = execute(&session, &pushdown.remote);
        assert_eq!(
            error.expect("the read fails").kind(),
            ErrorKind::Data,
            "{name}"
        );
    }

    // A read whose remote plan never returns those values succeeds.
    let offered = special_plan()
        .filter(call("equal", vec![column(0), literal(LiteralType::I32(3))]))
        .build();
    let pushdown = planned(&session, offered);
    assert!(pushdown.local_plan.is_none());
    let batch = execute_ok(&session, &pushdown.remote);
    assert_eq!(ids(&batch), [3]);
    assert_eq!(values::<Decimal128Array>(&batch, "amount").value(0), -1);
}

fn filtering_plan(filter: Expression) -> Plan {
    ReadPlan::new(
        [SCHEMA, "filtering"],
        vec![
            ("id", i32(false)),
            ("label", text(true)),
            ("code", text(true)),
            ("folded", text(true)),
            ("score", fp64(true)),
            ("external_id", logical("uuid", "{}", true)),
            ("c_label", text(true)),
            ("posix_label", text(true)),
            ("day", date(true)),
        ],
    )
    .filter(filter)
    .project(vec![(0, "id")])
    .build()
}

fn string(value: &str) -> Expression {
    literal(LiteralType::String(value.to_owned()))
}

fn filters_run_remotely_only_with_the_offered_meaning() {
    let session = session();
    let cases = [
        (
            "integer comparison",
            call("gt", vec![column(0), literal(LiteralType::I32(1))]),
            true,
            vec![2, 3],
        ),
        (
            "text equality on text",
            call("equal", vec![column(1), string("a")]),
            true,
            vec![1],
        ),
        (
            "uuid equality",
            call(
                "equal",
                vec![
                    column(5),
                    logical_text("uuid", "{}", "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
                ],
            ),
            true,
            vec![1],
        ),
        (
            "Boolean composition and null tests",
            call(
                "or",
                vec![
                    call("is_null", vec![column(5)]),
                    call(
                        "not",
                        vec![call("equal", vec![column(0), literal(LiteralType::I32(1))])],
                    ),
                ],
            ),
            true,
            vec![2, 3],
        ),
        (
            "text ordering depends on the collation",
            call("lt", vec![column(1), string("b")]),
            false,
            vec![],
        ),
        (
            "character(n) ignores trailing spaces",
            call("equal", vec![column(2), string("a")]),
            false,
            vec![],
        ),
        (
            "a nondeterministic collation",
            call("equal", vec![column(3), string("a")]),
            false,
            vec![],
        ),
        (
            "floating-point comparison differs for NaN",
            call("gt", vec![column(4), literal(LiteralType::Fp64(1.0))]),
            false,
            vec![],
        ),
        (
            "text equality between columns of one collation",
            call("equal", vec![column(6), column(6)]),
            true,
            vec![1, 2, 3],
        ),
        (
            "text equality between columns of different collations",
            call("equal", vec![column(6), column(7)]),
            false,
            vec![],
        ),
        (
            "a date literal before PostgreSQL's range",
            call("gt", vec![column(8), literal(LiteralType::Date(i32::MIN))]),
            false,
            vec![],
        ),
    ];
    for (name, filter, remote, expected) in cases {
        let pushdown = planned(&session, filtering_plan(filter));
        assert_eq!(pushdown.local_plan.is_none(), remote, "{name}");
        let batch = execute_ok(&session, &pushdown.remote);
        if remote {
            assert_eq!(ids(&batch), expected, "{name}");
        } else {
            // The whole table comes back for the local filter and projection.
            assert_eq!(ids(&batch), [1, 2, 3], "{name}");
            assert_eq!(batch.num_columns(), 9, "{name}");
        }
    }
}

fn remote_executions_run_in_another_session() {
    let planning = session();
    let offered = filtering_plan(call("equal", vec![column(1), string("b")]));
    let bytes = planned(&planning, offered).remote.to_bytes();
    drop(planning);

    let reading = session();
    let remote = RemoteExecution::<Read>::from_bytes(&bytes).unwrap();
    assert_eq!(ids(&execute_ok(&reading, &remote)), [2]);
}

fn altered_plan() -> Plan {
    ReadPlan::new(
        [SCHEMA, "altered"],
        vec![("id", i32(false)), ("value", i32(false))],
    )
    .build()
}

fn schema_changes_are_stale_registrations() {
    let session = session();
    let pushdown = planned(&session, altered_plan());

    server().psql(&format!(
        "ALTER TABLE {SCHEMA}.altered ALTER COLUMN value TYPE bigint, ALTER COLUMN value DROP NOT NULL"
    ));
    let (_, error) = execute(&session, &pushdown.remote);
    let error = error.expect("the read fails");
    assert_eq!(error.kind(), ErrorKind::StaleRegistration);
    let detail = |key: &str| {
        error
            .diagnostics()
            .iter()
            .find(|entry| entry.key() == key)
            .map(|entry| entry.value().clone())
    };
    assert_eq!(
        detail("column"),
        Some(DiagnosticValue::Utf8("value".to_owned()))
    );
    assert_eq!(
        detail("data_source_type"),
        Some(DiagnosticValue::Utf8("bigint".to_owned()))
    );
    assert_eq!(detail("nullable"), Some(DiagnosticValue::Bool(true)));

    server().psql(&format!("DROP TABLE {SCHEMA}.altered"));
    let (_, error) = execute(&session, &pushdown.remote);
    assert_eq!(error.unwrap().kind(), ErrorKind::StaleRegistration);
    assert!(matches!(
        plan(&session, altered_plan()),
        Err(PlanningError::Connector(error)) if error.kind() == ErrorKind::StaleRegistration
    ));
}

fn cancellation_and_deadlines_stop_the_query_at_the_source() {
    let session = session();
    let offered = ReadPlan::new([SCHEMA, "slow"], vec![("n", i32(true))]).build();
    let pushdown = planned(&session, offered);

    for stop in ["cancel", "expire"] {
        let mut stream = execute_planned(&pushdown.remote.output_schema, None, || {
            PostgresConnector::read(&session, &pushdown.remote)
        })
        .expect("execution starts");
        let handle = stream.cancel_handle();
        let started = Instant::now();
        let stopper = thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            match stop {
                "cancel" => handle.cancel(),
                _ => handle.expire(),
            }
        });
        let (_, error) = collect(&mut stream);
        stopper.join().unwrap().unwrap();
        let expected = if stop == "cancel" {
            ErrorKind::Cancellation
        } else {
            ErrorKind::Timeout
        };
        assert_eq!(error.unwrap().kind(), expected);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{stop} stopped the query"
        );
    }

    // The session accepts the next read once a stopped read is released.
    let pushdown = planned(&session, value_types_plan().build());
    assert_eq!(ids(&execute_ok(&session, &pushdown.remote)), [1, 2]);
}

fn errors_reveal_neither_statements_nor_secrets() {
    server().psql(&format!(
        "DROP ROLE IF EXISTS reader; \
         CREATE ROLE reader LOGIN PASSWORD 'reader-password'; \
         GRANT USAGE ON SCHEMA {SCHEMA} TO reader;"
    ));
    let library = library();
    let session = open(&library, options("reader", "reader-password"))
        .and_then(|data_source| open_session(&data_source))
        .expect("the session opens");
    let offered = filtering_plan(call("equal", vec![column(1), string("secret-literal")]));
    let pushdown = planned(&session, offered);
    let (_, error) = execute(&session, &pushdown.remote);
    let error = error.expect("the read fails");
    assert_eq!(error.kind(), ErrorKind::Permission);
    assert_reveals_nothing(&error, &["secret-literal", "SELECT", "reader-password"]);

    let error = open(&library, options("postgres", "wrong-password"))
        .and_then(|data_source| open_session(&data_source).map(|_| ()))
        .expect_err("authentication fails");
    // The driver reports a failed authentication without a SQLSTATE.
    assert_eq!(error.kind(), ErrorKind::Connection);
    assert_reveals_nothing(&error, &["wrong-password", PASSWORD]);
}

fn assert_reveals_nothing(error: &ConnectorError, secrets: &[&str]) {
    let mut texts = vec![
        error.to_string(),
        format!("{error:?}"),
        error.data_source_message().unwrap_or_default().to_owned(),
    ];
    for entry in error.diagnostics() {
        texts.push(match entry.value() {
            DiagnosticValue::Utf8(text) => text.clone(),
            DiagnosticValue::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            other => format!("{other:?}"),
        });
    }
    for text in texts {
        for secret in secrets {
            assert!(!text.contains(secret), "{secret} appears in {text}");
        }
    }
}
