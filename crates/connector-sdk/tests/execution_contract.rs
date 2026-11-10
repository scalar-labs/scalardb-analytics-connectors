mod support;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use connector_sdk::{
    CancellationToken, ConnectorError, ConnectorExecutor, ErrorKind, ExecutionContext,
    ExecutionOutput, PlannedPushdown, RecordBatchStream, execute_planned,
};
use futures_core::Stream;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};
use support::{planner::complete_pushdown, substrait_plan::requested_plan};

#[test]
fn execution_enforces_the_planned_schema_and_cancellation() {
    let pushdown = complete_pushdown(requested_plan());
    let schema = Arc::new(pushdown.remote_output_schema.to_arrow());
    let valid_batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![7])) as ArrayRef,
            Arc::new(StringArray::from(vec![Some(
                "550e8400-e29b-41d4-a716-446655440000",
            )])) as ArrayRef,
        ],
    )
    .unwrap();
    let executor = BatchExecutor { batch: valid_batch };
    let execution_context = ExecutionContext::new(CancellationToken::default(), None);
    let mut stream = execute_planned(&executor, &pushdown, execution_context).unwrap();
    let mut task_context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Stream::poll_next(Pin::new(&mut stream), &mut task_context),
        Poll::Ready(Some(Ok(batch))) if batch.num_rows() == 1
    ));

    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let error = execute_planned(
        &executor,
        &pushdown,
        ExecutionContext::new(cancellation, None),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(!error.retryable());
}

#[test]
fn execution_rejects_a_batch_with_a_different_schema() {
    let pushdown = complete_pushdown(requested_plan());
    let wrong_batch = RecordBatch::try_from_iter(vec![(
        "wrong",
        Arc::new(Int64Array::from(vec![7])) as ArrayRef,
    )])
    .unwrap();
    let executor = BatchExecutor { batch: wrong_batch };
    let mut stream = execute_planned(
        &executor,
        &pushdown,
        ExecutionContext::new(CancellationToken::default(), None),
    )
    .unwrap();
    let mut task_context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Stream::poll_next(Pin::new(&mut stream), &mut task_context),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Internal && !error.retryable()
    ));
}

struct BatchExecutor {
    batch: RecordBatch,
}

impl ConnectorExecutor for BatchExecutor {
    type Query = String;

    fn execute(
        &self,
        pushdown: &PlannedPushdown<String>,
        _context: &ExecutionContext,
    ) -> Result<ExecutionOutput, ConnectorError> {
        assert!(!pushdown.executable_query.query.is_empty());
        let stream: RecordBatchStream = Box::pin(OneBatchStream(Some(Ok(self.batch.clone()))));
        Ok(ExecutionOutput::new(stream))
    }
}

struct OneBatchStream(Option<Result<RecordBatch, ConnectorError>>);

impl Stream for OneBatchStream {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take())
    }
}
