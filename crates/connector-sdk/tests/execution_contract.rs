mod support;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use connector_sdk::{
    ConnectorError, ErrorKind, ExecutionCancel, ExecutionOutput, PortableQuery, RecordBatchStream,
    RemoteExecution, execute_planned,
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
    let remote = remote_execution();
    let executor = BatchExecutor::new(valid_batch(&remote));
    let mut stream = execute_planned(&remote.output_schema, None, || {
        executor.execute(&remote.query)
    })
    .unwrap();
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Ok(batch))) if batch.num_rows() == 1
    ));
    assert!(matches!(pull(&mut stream), Poll::Ready(None)));
}

#[test]
fn execution_rejects_a_batch_with_a_different_schema() {
    let remote = remote_execution();
    let wrong_batch = RecordBatch::try_from_iter(vec![(
        "wrong",
        Arc::new(Int64Array::from(vec![7])) as ArrayRef,
    )])
    .unwrap();
    let executor = BatchExecutor::new(wrong_batch);
    let mut stream = execute_planned(&remote.output_schema, None, || {
        executor.execute(&remote.query)
    })
    .unwrap();
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Internal && !error.retryable()
    ));
}

#[test]
fn cancelling_stops_the_connector_execution_and_ends_the_stream() {
    let remote = remote_execution();
    let executor = BatchExecutor::new(valid_batch(&remote));
    let mut stream = execute_planned(&remote.output_schema, None, || {
        executor.execute(&remote.query)
    })
    .unwrap();

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
    let remote = remote_execution();
    let executor = BatchExecutor::new(valid_batch(&remote));
    let mut stream = execute_planned(&remote.output_schema, None, || {
        executor.execute(&remote.query)
    })
    .unwrap();
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
    let remote = remote_execution();
    let executor = BatchExecutor::new(valid_batch(&remote));

    let error = execute_planned(&remote.output_schema, Some(Instant::now()), || {
        executor.execute(&remote.query)
    })
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::Timeout);

    let deadline = Instant::now() + Duration::from_millis(20);
    let mut stream = execute_planned(&remote.output_schema, Some(deadline), || {
        executor.execute(&remote.query)
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Timeout
    ));
}

#[test]
fn a_remote_execution_round_trips_through_bytes() {
    let remote = remote_execution();

    let restored = RemoteExecution::<TestQuery>::from_bytes(&remote.to_bytes()).unwrap();

    assert!(restored == remote);
    let executor = BatchExecutor::new(valid_batch(&restored));
    let mut stream = execute_planned(&restored.output_schema, None, || {
        executor.execute(&restored.query)
    })
    .unwrap();
    assert!(matches!(
        pull(&mut stream),
        Poll::Ready(Some(Ok(batch))) if batch.num_rows() == 1
    ));
}

#[test]
fn bytes_that_cannot_be_restored_are_an_invalid_request() {
    let bytes = remote_execution().to_bytes();

    for broken in [
        &bytes[..4],
        &bytes[..bytes.len() / 2],
        &b"not a remote execution"[..],
    ] {
        let error = RemoteExecution::<TestQuery>::from_bytes(broken)
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        assert!(!error.retryable());
    }
}

/// The remote part of the shared test pushdown, with a transportable query.
fn remote_execution() -> RemoteExecution<TestQuery> {
    let remote = complete_pushdown(requested_plan()).remote;
    RemoteExecution {
        query: TestQuery(remote.query),
        output_schema: remote.output_schema,
    }
}

fn valid_batch(remote: &RemoteExecution<TestQuery>) -> RecordBatch {
    let schema = Arc::new(remote.output_schema.to_arrow());
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

/// A test query carried as UTF-8 text.
#[derive(Clone, PartialEq, Eq)]
struct TestQuery(String);

impl PortableQuery for TestQuery {
    fn to_bytes(&self) -> Vec<u8> {
        self.0.as_bytes().to_vec()
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, ConnectorError> {
        String::from_utf8(bytes.to_vec())
            .map(Self)
            .map_err(|_| ConnectorError::new(ErrorKind::InvalidRequest, false, "not UTF-8"))
    }
}

impl BatchExecutor {
    fn execute(&self, query: &TestQuery) -> Result<ExecutionOutput, ConnectorError> {
        assert!(!query.0.is_empty());
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
