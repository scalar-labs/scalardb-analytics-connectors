# Transfer ADBC query resources to blocking polls

## Design question

Should `AdbcQueryStream` keep query resources inside its shared `QueryState`
while a blocking source call is in progress, or temporarily transfer those
resources to the polling thread?

## Decision

Transfer the resources required by a blocking source call to the polling thread
for the duration of `poll_next`.

`Pending` owns the query request between reads, and `Ready` owns the result
reader and statement between reads. Starting a read atomically moves that
resource into a one-call `ReadAction` and changes the shared state to
`Preparing` or `Reading`. The polling thread owns the transferred resource until
the blocking call completes. A successful batch read returns the execution to
`Ready`; cancellation, end of stream, and errors release it.

During an in-progress operation, the shared state records the lifecycle phase
and, once a statement exists, a clone of its statement-scoped `CancelHandle`.
The cancellation handle uses that capability without taking ownership of the
statement or reader.

## Shared state ownership and access

`AdbcQueryStream` creates and retains one strong reference to the
`Arc<Mutex<QueryState>>`. Every `AdbcQueryCancelHandle` cloned from the stream
retains another strong reference to the same allocation. The query state
therefore remains available until both the stream and all of its cancellation
handles have been dropped.

The `Mutex` protects only `QueryState`. It does not protect resources that have
been transferred to the polling thread for a blocking call.

| Participant | Shared ownership | Access while holding the state mutex | Access outside the state mutex |
| --- | --- | --- | --- |
| `AdbcQueryStream` | Retains one `Arc` reference for the stream lifetime | Starts a read by moving the resource out of `Pending` or `Ready`; records completion, cancellation observation, and cleanup transitions | The polling thread exclusively owns and uses the transferred request, statement, or execution until the blocking call returns |
| `AdbcQueryCancelHandle` | Each clone retains one `Arc` reference | Inspects the current phase, records local cancellation, or invokes the phase's statement-scoped cancel handle before recording `Cancelled` | None |
| `AdbcQueryStream::drop` | Uses the stream's existing `Arc` reference | Replaces the current state with `Finished` | Drops the replaced state after releasing the mutex so resource destructors do not run inside the critical section |

The state mutex is never held across ordinary source work such as statement
creation, query setup, execution, or batch reading. This allows a cancellation
handle on another thread to observe the in-progress phase. Native cancellation
is invoked while holding the state mutex so cancellation and stream cleanup
cannot release the statement concurrently with its cancel callback.

## Rationale

Source access and `RecordBatchReader::next` are synchronous. `poll_next` blocks
the calling thread until the current source operation returns and does not yield
`Poll::Pending`. The polling thread is therefore the natural temporary owner of
the resources used by that operation.

Keeping those resources inside the shared state would require an additional
reference-counted, mutex-protected, optional resource container for every
in-progress operation. The outer query-state mutex still could not be held
during a source call because cancellation must acquire it from another thread.
Transferring ownership expresses the blocking execution model directly and
keeps synchronization limited to `Arc<Mutex<QueryState>>`.

## Consequences

The shared `QueryState` owns complete query resources in stable `Pending` and
`Ready` states. During `Preparing` and `Reading`, ownership is split deliberately:
the shared state records the in-progress phase and cancellation capability,
while the polling thread owns the request, statement, or execution.

This design relies on `Stream::poll_next` receiving exclusive access to the
stream and on the implementation completing each blocking operation before
returning. A second read cannot start while the first read owns the resources.
Cancellation may change the shared state to `Cancelled`, but the polling thread
keeps the transferred resources alive until the source call returns and then
releases them.

The model avoids nested resource mutexes and per-read reference-counted
operation objects. Code that changes polling to yield `Poll::Pending` must
revisit this ownership decision because the transferred resources would then
need storage that survives the call to `poll_next`.
