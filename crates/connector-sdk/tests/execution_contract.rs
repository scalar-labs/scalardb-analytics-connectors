mod support;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use connector_sdk::{
    CancellationToken, ConnectorError, ConnectorExecutor, ErrorKind, ExecutionContext,
    ExecutionOutput, ExecutionProvenance, PlannedSubplan, RecordBatchStream, StreamLimits,
    execute_planned,
};
use futures_core::Stream;
use prost::Message;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Waker},
};
use support::{
    planner::{context, exact_plan},
    substrait_plan::candidate_plan,
};

#[test]
fn execution_enforces_the_planned_schema_and_cancellation() {
    let bytes = candidate_plan().encode_to_vec();
    let planned = exact_plan(&bytes, context("binary"));
    let schema = Arc::new(planned.output_schema().to_arrow());
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
    let executor = BatchExecutor {
        batch: valid_batch,
        fingerprint: planned.validation_fingerprint(),
    };
    let execution_context = ExecutionContext::new(
        CancellationToken::default(),
        None,
        StreamLimits::new(10, 1024 * 1024).unwrap(),
    );
    let mut stream = execute_planned(&executor, &planned, execution_context).unwrap();
    let mut task_context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Stream::poll_next(Pin::new(&mut stream), &mut task_context),
        Poll::Ready(Some(Ok(batch))) if batch.num_rows() == 1
    ));

    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let error = execute_planned(
        &executor,
        &planned,
        ExecutionContext::new(cancellation, None, StreamLimits::default()),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind(), ErrorKind::Cancellation);
    assert!(!error.retryable());
}

#[test]
fn execution_rejects_a_batch_with_a_different_schema() {
    let bytes = candidate_plan().encode_to_vec();
    let planned = exact_plan(&bytes, context("binary"));
    let wrong_batch = RecordBatch::try_from_iter(vec![(
        "wrong",
        Arc::new(Int64Array::from(vec![7])) as ArrayRef,
    )])
    .unwrap();
    let executor = BatchExecutor {
        batch: wrong_batch,
        fingerprint: planned.validation_fingerprint(),
    };
    let mut stream = execute_planned(
        &executor,
        &planned,
        ExecutionContext::new(CancellationToken::default(), None, StreamLimits::default()),
    )
    .unwrap();
    let mut task_context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Stream::poll_next(Pin::new(&mut stream), &mut task_context),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Internal && !error.retryable()
    ));
}

#[test]
fn stream_limits_reject_invalid_library_settings() {
    let error = StreamLimits::new(0, 1024).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::InvalidSettings);
    assert!(!error.retryable());
}

#[test]
fn execution_enforces_batch_resource_limits() {
    let bytes = candidate_plan().encode_to_vec();
    let planned = exact_plan(&bytes, context("binary"));
    let batch = RecordBatch::try_new(
        Arc::new(planned.output_schema().to_arrow()),
        vec![
            Arc::new(Int64Array::from(vec![7])) as ArrayRef,
            Arc::new(StringArray::from(vec![Some("external-id")])) as ArrayRef,
        ],
    )
    .unwrap();
    let executor = BatchExecutor {
        batch,
        fingerprint: planned.validation_fingerprint(),
    };
    let mut stream = execute_planned(
        &executor,
        &planned,
        ExecutionContext::new(
            CancellationToken::default(),
            None,
            StreamLimits::new(10, 1).unwrap(),
        ),
    )
    .unwrap();
    let mut task_context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Stream::poll_next(Pin::new(&mut stream), &mut task_context),
        Poll::Ready(Some(Err(error))) if error.kind() == ErrorKind::Resource && error.retryable()
    ));
}

struct BatchExecutor {
    batch: RecordBatch,
    fingerprint: connector_sdk::ValidationFingerprint,
}

impl ConnectorExecutor for BatchExecutor {
    fn execute(
        &self,
        _plan: &PlannedSubplan,
        _context: &ExecutionContext,
    ) -> Result<ExecutionOutput, ConnectorError> {
        let stream: RecordBatchStream = Box::pin(OneBatchStream(Some(Ok(self.batch.clone()))));
        Ok(ExecutionOutput::new(
            stream,
            ExecutionProvenance::new(
                "test",
                "postgresql",
                "17.2",
                Some("snapshot-42".to_owned()),
                self.fingerprint,
            ),
        ))
    }
}

struct OneBatchStream(Option<Result<RecordBatch, ConnectorError>>);

impl Stream for OneBatchStream {
    type Item = Result<RecordBatch, ConnectorError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.take())
    }
}
