//! The connector conformance suite: checks that a Connector meets the
//! connector contract (`docs/connector-contract.md`) through the
//! caller-facing [`runtime`](crate::runtime) alone.
//!
//! The suite knows no Connector. A connector crate implements [`Fixture`] for
//! its Data Source in its own tests, enables this crate's `conformance`
//! feature as a dev-dependency, and calls [`run`] from one test:
//!
//! ```ignore
//! #[test]
//! fn the_connector_meets_the_contract() {
//!     connector_sdk::conformance::run(&MyFixture::start());
//! }
//! ```
//!
//! [`run`] initializes a [`Library`] with only [`Fixture::Connector`] built
//! into it and drives the real connector against the real source, exactly as
//! `connector-api` does behind the C ABI; nothing is mocked. The fixture
//! prepares the source, states what the contract expects of each request, and
//! performs the source-side operations the suite cannot perform through the
//! contract, such as making a request wait at the source and observing
//! whether it still runs there.
//!
//! # What the suite checks
//!
//! In this order, each part panicking with the name of the failing case:
//!
//! 1. Metadata: every [`MetadataCase`], with exact name filters, the
//!    determinism of table metadata, and every source metadata document
//!    conforming to the schema the static information publishes
//!    ([`BuiltInConnector::source_metadata_schema`]).
//! 2. Reads: every [`ReadCase`], its planning outcome and remote result.
//! 3. Schema drift: every [`DriftCase`], a source change between planning
//!    and reading.
//! 4. Errors: every [`ErrorCase`], its error kind and retry flag.
//! 5. Interruptions: every [`Interruption`], through the fixture's
//!    source-side operations; the metadata request interruptions only where
//!    the source can make metadata requests wait ([`Fixture::block_metadata`]).
//! 6. Secrets and literals: no error received in parts 1 to 5 reveals a value
//!    of an option the connector's descriptor marks secret, or a string
//!    literal of its request's filters.
//!
//! Properties that do not depend on the Connector, such as release order or
//! rejecting a plan that is not valid Substrait, are tested once by the SDK
//! and the connector API and are not part of this suite.
//!
//! # Known deviations
//!
//! A behavior a connector does not yet meet is stated, not left out: an
//! [`Expected::KnownDeviation`] for a result, or an entry of
//! [`Fixture::known_interruption_deviations`] for an interruption, each
//! recorded in the connector's README as a known limitation. The suite checks
//! the deviating behavior as stated and fails once the connector meets the
//! contract, so that a fix also updates the fixture and the README.

mod errors;
mod metadata;
pub mod plan;
mod read;
mod stop;

use std::{fmt::Debug, sync::Arc, time::Duration};

use arrow_array::ArrayRef;
use substrait::proto::Plan;

use crate::{
    Connector, ConnectorError, ErrorKind, LibrarySettings, Listed, NameFilter, NamespacePath,
    SecretString, TableEntry, TableMetadata, TableReference,
    runtime::{BuiltInConnector, DataSourceState, Library, SessionState},
};

use errors::Observed;

/// The library version the suite reports in retrieval info.
const LIBRARY_VERSION: &str = "conformance";

/// How long opening a Data Source and its sessions may take.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a request has waited at the source when the suite cancels it or
/// breaks its connection.
pub const STOP_DELAY: Duration = Duration::from_millis(500);

/// The timeout the suite gives the requests it lets expire.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the source may take to end a request after it was interrupted.
///
/// A known interruption deviation is confirmed by the source query still
/// running after this long, so each one adds this much to the run.
pub const SOURCE_STOP_BOUND: Duration = Duration::from_secs(10);

/// One Connector's Data Source prepared for the suite, with what the contract
/// expects of it.
///
/// # Lifecycle
///
/// [`run`] calls [`Fixture::prepare`] once, first; every other method may
/// assume it ran. Each case method is called once, when its part of the suite
/// starts, so a case may embed values read from the prepared source, such as
/// object identifiers. The source-side operations are called once or more
/// each, during the interruption checks.
///
/// # Credentials
///
/// [`Fixture::options`] and the options of every case are the fixture's
/// credentials. [`Fixture::source_is_idle`] and
/// [`Fixture::break_connections`] concern exactly the source connections of
/// those credentials, so the fixture's own administrative access, such as the
/// one that prepares the source or holds a [`Fixture::MetadataBlock`], must
/// use another credential.
///
/// # Threads
///
/// The suite calls [`Fixture::break_connections`] from another thread while a
/// pull waits on the suite's thread, so a fixture is `Sync`.
pub trait Fixture: Sync {
    /// The Connector under test; the only connector built into the library.
    type Connector: Connector;

    /// A block at the source that makes table metadata requests wait while
    /// the value is alive and releases them when it is dropped.
    type MetadataBlock;

    /// The Connector name passed when opening Data Sources; one of the names
    /// in the connector descriptor's `connectors`.
    fn connector_name(&self) -> &str;

    /// The settings the library is initialized with, such as the directory of
    /// the connector's ADBC driver.
    fn library_settings(&self) -> LibrarySettings;

    /// Connection options of a credential that may read every object the
    /// fixture prepares.
    ///
    /// The suite opens a new Data Source and session with them for each part
    /// and each interruption check.
    fn options(&self) -> Vec<(String, SecretString)>;

    /// Creates the fixture's objects at the source, replacing every object of
    /// an earlier run, so that each run starts from the same source state.
    ///
    /// It creates whatever the cases and source-side operations below refer
    /// to, including the credentials and their privileges. A failure panics.
    fn prepare(&self);

    /// The metadata requests, together covering every mapping rule of the
    /// connector's type mapping, unmapped types, constraints, targets without
    /// privilege, nonexistent tables, several namespaces, and names that
    /// differ only in case.
    fn metadata_cases(&self) -> Vec<MetadataCase>;

    /// The offered regions, together covering the boundary values of every
    /// covered mapping rule (minimum, maximum, precision limit, time zones,
    /// and NULL), values that cannot be carried exactly, and every rewrite
    /// rule fully remote, with local work, and unsupported.
    fn read_cases(&self) -> Vec<ReadCase>;

    /// The source changes between planning and reading.
    ///
    /// Each case changes an object of its own, because the suite does not
    /// undo a change before the next case.
    fn drift_cases(&self) -> Vec<DriftCase<'_>>;

    /// The requests that fail, such as reading a table without privilege or
    /// opening a session with a rejected password, each with the credential
    /// it uses.
    fn error_cases(&self) -> Vec<ErrorCase>;

    /// A read whose first pull waits at the source for longer than
    /// [`SOURCE_STOP_BOUND`].
    ///
    /// The suite cancels it after [`STOP_DELAY`], lets it expire after
    /// [`REQUEST_TIMEOUT`], and breaks its connection while it waits.
    fn waiting_read(&self) -> Plan;

    /// A read whose first batch arrives within [`REQUEST_TIMEOUT`] while the
    /// source keeps producing rows for longer than [`SOURCE_STOP_BOUND`].
    ///
    /// The suite releases its stream after the first batch.
    fn long_read(&self) -> Plan;

    /// Blocks table metadata requests for the returned table at the source
    /// until the returned block is dropped, and returns once the block holds.
    ///
    /// A request for the table must then wait for longer than
    /// [`SOURCE_STOP_BOUND`]. The suite cancels one such request after
    /// [`STOP_DELAY`] and lets another expire after [`REQUEST_TIMEOUT`],
    /// checking [`Fixture::source_is_idle`] before dropping the block, so the
    /// block itself must not count as a running request.
    ///
    /// `None` when nothing at the source can make the connector's metadata
    /// reads wait, such as catalogs read without locks. A metadata request
    /// there never waits at the source, so there is no waiting request to
    /// interrupt, and the suite skips
    /// [`Interruption::CancelledMetadataRequest`] and
    /// [`Interruption::ExpiredMetadataRequest`]. The connector's README
    /// records why.
    fn block_metadata(&self) -> Option<(TableReference, Self::MetadataBlock)>;

    /// Whether no request of the fixture's credentials is running at the
    /// source.
    ///
    /// The suite polls it after interrupting a request: the contract requires
    /// it to become true within [`SOURCE_STOP_BOUND`]. An open but idle
    /// connection does not count as running.
    fn source_is_idle(&self) -> bool;

    /// Breaks every source connection of the fixture's credentials, as a
    /// network failure or a server restart would.
    ///
    /// The suite calls it from another thread while a pull of
    /// [`Fixture::waiting_read`] waits, and also to clean up after a known
    /// interruption deviation leaves a source query running.
    fn break_connections(&self);

    /// The interruptions the connector is known to handle against the
    /// contract, each with the reason its README records. Empty when the
    /// connector meets the contract for every interruption.
    fn known_interruption_deviations(&self) -> Vec<(Interruption, &'static str)>;
}

/// A request interrupted through the contract or at the source.
///
/// After a cancellation, an expiry, or a release the source query must end
/// within [`SOURCE_STOP_BOUND`] and the session must accept the next request;
/// after a broken connection the session must reject the next request as
/// `invalid state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interruption {
    /// A pull of [`Fixture::waiting_read`] cancelled from another thread.
    CancelledPull,
    /// [`Fixture::waiting_read`] started with [`REQUEST_TIMEOUT`].
    ExpiredRead,
    /// [`Fixture::long_read`] released after its first batch.
    ReleasedStream,
    /// A table metadata request blocked by [`Fixture::block_metadata`],
    /// cancelled from another thread.
    CancelledMetadataRequest,
    /// A table metadata request blocked by [`Fixture::block_metadata`], with
    /// [`REQUEST_TIMEOUT`].
    ExpiredMetadataRequest,
    /// The connections broken by [`Fixture::break_connections`] while a pull
    /// of [`Fixture::waiting_read`] waits.
    BrokenConnection,
}

/// The result a case expects: the contract's, or a known deviation from it.
///
/// A deviation is checked as observed, so that it is fixed visibly: once the
/// connector returns the contract's result, the case fails and the fixture
/// must state [`Expected::Contract`] instead.
#[derive(Clone, Debug)]
pub enum Expected<T> {
    /// The contract's result.
    Contract(T),
    /// The contract requires `contract`, but the connector returns `observed`.
    KnownDeviation {
        contract: T,
        observed: T,
        /// Why the connector deviates, as its README records it.
        reason: &'static str,
    },
}

impl<T> Expected<T> {
    /// The result the connector is checked against.
    fn observed(&self) -> &T {
        match self {
            Self::Contract(expected) => expected,
            Self::KnownDeviation { observed, .. } => observed,
        }
    }

    /// The result the contract requires, when the connector is known to deviate.
    fn deviation(&self) -> Option<&T> {
        match self {
            Self::Contract(_) => None,
            Self::KnownDeviation { contract, .. } => Some(contract),
        }
    }
}

/// One metadata request and its expected result.
pub struct MetadataCase {
    /// Names the case in failure messages.
    pub name: &'static str,
    /// The credential the request uses, such as one without privilege on the
    /// requested tables.
    pub options: Vec<(String, SecretString)>,
    pub request: MetadataRequest,
}

/// A metadata request and the result it must return, compared exactly.
pub enum MetadataRequest {
    /// A namespace list; `expected` holds every listed item in the order the
    /// connector returns them.
    Namespaces {
        filter: NameFilter,
        expected: Vec<Listed<NamespacePath>>,
    },
    /// A table list; `expected` holds every listed item in the order the
    /// connector returns them.
    Tables {
        namespaces: NameFilter,
        tables: NameFilter,
        expected: Vec<Listed<TableEntry>>,
    },
    /// A table metadata request, sent twice: both retrievals must return the
    /// same records and source metadata documents, apart from retrieval info.
    TableMetadata {
        tables: Vec<TableReference>,
        /// One record per requested table, in request order.
        expected: Vec<ExpectedRecord>,
    },
}

/// The expected record of one requested table, apart from retrieval info and
/// source metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpectedRecord {
    /// The table was read; its table metadata is compared as a whole,
    /// including source identifiers and the representation of missing
    /// information.
    Read(TableMetadata),
    /// The connector confirmed that the table does not exist.
    DoesNotExist,
    /// The table exists, but the credential may not read it.
    PermissionDenied,
    /// The table was not found, and whether it exists is unknown.
    NotFound,
}

/// One offered region and what planning and reading it return.
pub struct ReadCase {
    /// Names the case in failure messages.
    pub name: &'static str,
    /// The offered region as an Adapter would send it; [`plan`] builds one.
    pub offered: Plan,
    pub expected: ExpectedPlanning,
}

/// What planning one offered region returns.
pub enum ExpectedPlanning {
    /// The connector cannot run any of the region remotely.
    Unsupported,
    /// The connector planned the region, and its remote execution is read to
    /// the end with no timeout.
    Planned {
        /// Whether a local plan remains above the remote result.
        local_work: bool,
        /// What reading the remote execution returns. Without local work it
        /// is the region's result; with local work it is what the local plan
        /// would run over.
        result: Expected<ReadResult>,
    },
}

/// What reading one remote execution returns.
#[derive(Clone, Debug, PartialEq)]
pub enum ReadResult {
    /// Every row, as one array per output-schema column, in the order the
    /// source returns them.
    ///
    /// The arrays are compared exactly, including their Arrow data types, so
    /// a timestamp with a time zone carries `UTC` as the output schema does.
    Rows(Vec<ArrayRef>),
    /// The read fails with this error.
    Error(ExpectedError),
}

/// An error's kind and retry flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpectedError {
    pub kind: ErrorKind,
    pub retryable: bool,
}

/// A source change between planning a read and reading it on the same
/// session.
pub struct DriftCase<'a> {
    /// Names the case in failure messages.
    pub name: &'static str,
    /// A region that plans as supported before the change.
    pub offered: Plan,
    /// Changes the source, through the fixture's administrative access.
    pub change: Box<dyn Fn() + 'a>,
    pub expected: DriftOutcome,
}

/// What reading after a [`DriftCase`]'s change returns.
pub enum DriftOutcome {
    /// The read fails with this kind, such as `does not exist` or `schema
    /// mismatch` for a stale registration, or `not found` where the source
    /// hides what the credential may not read.
    Fails(ErrorKind),
    /// The read succeeds with the schema it was planned with.
    Reads,
}

/// A request that fails, with the credential it uses.
pub struct ErrorCase {
    /// Names the case in failure messages.
    pub name: &'static str,
    /// The credential the request uses; its secret values are among those no
    /// error may reveal.
    pub options: Vec<(String, SecretString)>,
    pub request: FailingRequest,
    pub expected: Expected<ExpectedError>,
}

/// The request of an [`ErrorCase`].
pub enum FailingRequest {
    /// Opening a Data Source and a session; either may fail.
    OpenSession,
    /// Planning the region and reading its remote execution; either may fail.
    /// A string literal in its filter is among the values no error may
    /// reveal.
    Read(Box<Plan>),
}

/// Runs every part of the suite against `fixture`; see the [module
/// documentation](self) for what it checks.
///
/// It panics at the first failing case, naming it.
pub fn run<F: Fixture>(fixture: &F) {
    fixture.prepare();
    let probe = Probe::new(fixture);
    metadata::check(&probe);
    read::check(&probe);
    read::check_drift(&probe);
    errors::check(&probe);
    stop::check(&probe);
    probe.observed.check();
}

/// The library opened for one suite run, and the errors it observed.
struct Probe<'a, F: Fixture> {
    fixture: &'a F,
    library: Arc<Library>,
    observed: Observed,
}

impl<'a, F: Fixture> Probe<'a, F> {
    fn new(fixture: &'a F) -> Self {
        let library = Library::initialize(
            &[BuiltInConnector::of::<F::Connector>()],
            &fixture.library_settings(),
            LIBRARY_VERSION,
        )
        .expect("the library initializes");
        Self {
            fixture,
            library,
            observed: Observed::new(&F::Connector::descriptor()),
        }
    }

    fn data_source(
        &self,
        options: Vec<(String, SecretString)>,
    ) -> Result<Arc<DataSourceState>, ConnectorError> {
        self.observed.secrets(&options);
        let result = self.library.open_data_source(
            self.fixture.connector_name(),
            options,
            Some(CONNECTION_TIMEOUT),
        );
        self.observed.result(result, &[])
    }

    /// Opens a session with `options`.
    fn session_with(
        &self,
        options: Vec<(String, SecretString)>,
    ) -> Result<Arc<SessionState>, ConnectorError> {
        let data_source = self.data_source(options)?;
        let result = data_source.open_session();
        self.observed.result(result, &[])
    }

    /// Opens a session with the fixture's credential.
    fn session(&self) -> Arc<SessionState> {
        self.session_with(self.fixture.options())
            .expect("a session opens with the fixture's credential")
    }
}

/// Fails with `case` when `actual` is not `expected`.
fn assert_case<T: PartialEq + Debug>(case: &str, actual: &T, expected: &Expected<T>) {
    if let Some(contract) = expected.deviation() {
        assert!(
            actual != contract,
            "{case}: the connector now meets the contract; state the contract's result instead of the known deviation"
        );
    }
    assert_eq!(actual, expected.observed(), "{case}");
}
