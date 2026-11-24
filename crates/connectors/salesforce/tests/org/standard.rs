//! Values of the standard fields whose types a custom field cannot have, read
//! from the Task `run.sh prepare` creates: `Subject` is a `combobox` and
//! `CallDurationInSeconds` an `int`.
//!
//! An org holds other Tasks, so the read selects the test Task by a number
//! only it has, with a filter that runs remotely, and reads no other field.

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, Int32Array, StringArray};
use connector_sdk::{conformance::plan::*, runtime::SessionState};
use substrait::proto::expression::literal::LiteralType;

use super::{NAMESPACE, Org, reads::read};

/// `Task.CallDurationInSeconds` of the test Task, whose `Subject` is
/// [`TASK_SUBJECT`].
const TASK_CALL_DURATION: i32 = 87_654_321;
const TASK_SUBJECT: &str = "Connector Test Task";

pub fn check(org: &Org) {
    let session = org.session();
    a_task_reads_its_combobox_and_int(&session);
    session.close();
}

fn a_task_reads_its_combobox_and_int(session: &Arc<SessionState>) {
    let offered = ReadPlan::new(
        [NAMESPACE, "Task"],
        vec![
            ("Subject", text(true)),
            ("CallDurationInSeconds", i32(true)),
        ],
    )
    .filter(call(
        "equal",
        vec![column(1), literal(LiteralType::I32(TASK_CALL_DURATION))],
    ))
    .build();
    let columns = read(session, offered);
    let durations: Vec<_> = downcast::<Int32Array>(&columns[1]).iter().collect();
    assert!(
        durations
            .iter()
            .all(|value| *value == Some(TASK_CALL_DURATION)),
        "Task.CallDurationInSeconds = {TASK_CALL_DURATION} returns {durations:?}"
    );
    let subjects: Vec<_> = downcast::<StringArray>(&columns[0]).iter().collect();
    assert!(
        subjects.contains(&Some(TASK_SUBJECT)),
        "no Task {TASK_SUBJECT:?} among {subjects:?}; run `run.sh prepare`"
    );
}

fn downcast<A: Array + 'static>(column: &ArrayRef) -> &A {
    column
        .as_any()
        .downcast_ref::<A>()
        .expect("the column has its mapped Arrow type")
}
