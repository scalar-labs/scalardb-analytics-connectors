mod support;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use connector_sdk::{
    ConnectorError, ConnectorExecutor, ErrorKind, ExecutionCancel, ExecutionOutput,
    PlannedPushdown, RecordBatchStream, execute_planned,
};
use futures_core::Stream;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use support::{planner::complete_pushdown, substrait_plan::requested_plan};

#[test]
fn execution_enforces_the_planned_schema() {
    let pushdown = complete_pushdown(requested_plan());
    let executor = BatchExecutor::new(valid_batch(&pushdown));
    let mut stream = execute_planned(&executor, &pushdown, None).unwrap();
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Ok(batch))) if batch.num_rows() == 1
    ));
    assert!(matches!(pull(&mut stream), Poll::Ready(None)));
}

#[test]
fn execution_rejects_a_batch_with_a_different_schema() {
    let pushdown = complete_pushdown(requested_plan());
    let wrong_batch = RecordBatch::try_from_iter(vec![(
        "wrong",
        Arc::new(Int64Array::from(vec![7])) as ArrayRef,
    )])
    .unwrap();
    let executor = BatchExecutor::new(wrong_batch);
    let mut stream = execute_planned(&executor, &pushdown, None).unwrap();
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Internal && !error.retryable()
    ));
}

#[test]
fn cancelling_stops_the_connector_execution_and_ends_the_stream() {
    let pushdown = complete_pushdown(requested_plan());
    let executor = BatchExecutor::new(valid_batch(&pushdown));
    let mut stream = execute_planned(&executor, &pushdown, None).unwrap();

    stream.cancel_handle().cancel().unwrap();

    assert_eq!(executor.cancel_calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Cancellation && !error.retryable()
    ));
    assert!(matches!(pull(&mut stream), Poll::Ready(None)));
}

#[test]
fn expiry_reports_timeout_and_the_first_stop_reason_wins() {
    let pushdown = complete_pushdown(requested_plan());
    let executor = BatchExecutor::new(valid_batch(&pushdown));
    let mut stream = execute_planned(&executor, &pushdown, None).unwrap();
    let handle = stream.cancel_handle();

    handle.expire().unwrap();
    handle.cancel().unwrap();

    assert_eq!(executor.cancel_calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Timeout && error.retryable()
    ));
}

#[test]
fn a_passed_deadline_is_a_timeout() {
    let pushdown = complete_pushdown(requested_plan());
    let executor = BatchExecutor::new(valid_batch(&pushdown));

    let error = execute_planned(&executor, &pushdown, Some(Instant::now()))
        .err()
        .unwrap();
    assert_eq!(error.kind(), ErrorKind::Timeout);

    let deadline = Instant::now() + Duration::from_millis(20);
    let mut stream = execute_planned(&executor, &pushdown, Some(deadline)).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Timeout
    ));
}

fn valid_batch(pushdown: &PlannedPushdown<String>) -> RecordBatch {
    let schema = Arc::new(pushdown.remote_output_schema.to_arrow());
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![7])) as ArrayRef,
            Arc::new(StringArray::from(vec![Some(
                "550e8400-e29b-41d4-a716-446655440000",
            )])) as ArrayRef,
        ],
    )
    .unwrap()
}

fn pull(
    stream: &mut connector_sdk::ExecutionStream,
) -> Poll<Option<Result<RecordBatch, ConnectorError>>> {
    let mut task_context = Context::from_waker(Waker::noop());
    Stream::poll_next(Pin::new(stream), &mut task_context)
}

struct BatchExecutor {
    batch: RecordBatch,
    cancel_calls: Arc<AtomicUsize>,
}

impl BatchExecutor {
    fn new(batch: RecordBatch) -> Self {
        Self {
            batch,
            cancel_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ConnectorExecutor for BatchExecutor {
    type Query = String;

    fn execute(
        &self,
        pushdown: &PlannedPushdown<String>,
    ) -> Result<ExecutionOutput, ConnectorError> {
        assert!(!pushdown.executable_query.query.is_empty());
        let stream: RecordBatchStream = Box::pin(OneBatchStream(Some(Ok(self.batch.clone()))));
        let cancel = Arc::new(CountingCancel(Arc::clone(&self.cancel_calls)));
        Ok(ExecutionOutput::new(stream, cancel))
    }
}

struct CountingCancel(Arc<AtomicUsize>);

impl ExecutionCancel for CountingCancel {
    fn cancel(&self) -> Result<(), ConnectorError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct OneBatchStream(Option<Result<RecordBatch, ConnectorError>>);

impl Stream for OneBatchStream {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take())
    }
}
