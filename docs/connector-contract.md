# Connector contract

## Summary

The connector contract is what Query Engine adapters and the Physical Catalog
may rely on when they call a connector. It defines the two capabilities, the
calling conventions shared by every request, metadata retrieval, data read,
the representation of read requests, and how these map onto the connector API.
It does not define how a connector is built; that belongs to
[Connector internals](connector-internals.md) and
[Connector implementation conventions](connector-implementation.md).

This document is the semantic source of truth for the connector boundary.
Analytics types are defined by the [Analytics type model](analytics-types.md),
and per-source mappings by [Type mappings](type-mappings.md). Runtime formats
and generated bindings must preserve the behavior defined here.

## Capabilities

There is one Connector per supported system. Only generic ODBC and
Query Engine-backed Connectors handle several kinds of Data Source each
([Support tiers](#support-tiers)). A connector has two capabilities.

**Metadata retrieval** reads namespaces, tables, columns, Data Source types, and
constraints from the source's catalog and decides an Analytics type for each
column ([Metadata retrieval](#metadata-retrieval)). It consists of three
requests:

| Request | Input | Output |
| --- | --- | --- |
| Namespace list | A namespace name filter | Namespace names, and ranges whose list could not be read |
| Table list | A namespace filter and a table name filter; no namespace filter means all namespaces | Each table's namespace, name, and kind, and ranges whose list could not be read |
| Table metadata | A set of tables | One table metadata record per requested table, including tables that do not exist or could not be read ([Missing information](#missing-information)) |

**Data read** returns the rows of one table as Arrow typed by Analytics types
([Data read](#data-read)). It consists of one request:

| Request | Input | Output |
| --- | --- | --- |
| Planned read | A connector-accepted Substrait subplan and its validated planning state ([Read request representation](#read-request-representation)) | An Arrow stream with the planned schema, plus operation-owned read provenance retrieved after execution ([Read provenance](#read-provenance)) |

Both capabilities follow the [calling conventions](#calling-conventions).
Before receiving requests, a connector states which part of the two
capabilities it can serve in its
[capability declaration](#capability-declaration). How inputs and outputs
cross the API is defined in [Connector API](#connector-api).

## Connector API and Data Source access

Callers use a connector through the connector API: a typed C ABI exposed by the
connector API library, which contains the connector SDK and the connectors
built into it. Results cross it through the Arrow C data interface. A caller
names a Connector when it opens a data source, and the connector for that
Connector handles all later requests on it. The reason the connector API is
not ADBC is recorded in
[Expose a typed C ABI instead of ADBC](design/expose-a-typed-c-abi-instead-of-adbc.md).

Data Source access is the part of a Connector that reads the Data Source's catalog,
sends queries to the Data Source, and receives the results as Arrow. It takes one
of two forms, and a Connector uses exactly one form per supported system:

- **ADBC access** goes through an ADBC (Arrow Database Connectivity) driver.
  An existing driver for the source is used when one exists, such as the
  Apache Arrow ADBC project's PostgreSQL driver. Otherwise we write our own
  ADBC driver that handles only connecting, querying, receiving Arrow, and
  cancellation. ADBC API 1.1.0 or later is required because cancellation and
  detailed errors were introduced in 1.1.0.
- **Direct access** is performed by the connector itself, through Rust crates
  that return Arrow (reader crates, such as `parquet`, `iceberg-rust`, and
  `delta-rs`) or a client for the source's API.

~~~text
adapter / Physical Catalog
   | connector API (C ABI)
   v
connector SDK + Connectors (one library)
   |
   +- ADBC access ---> ADBC driver (separate artifact per source) ---> data source
   |
   +- direct access (reader crates built into the library) ---------> data source
~~~

The form is chosen from the source's nature, in this order:

1. A source that needs a client library with native code uses ADBC access,
   even if rule 3 would otherwise apply. Direct access is built into the
   library and is written in Rust without native dependencies, while an ADBC
   driver is a separate artifact that can contain its native dependencies.
   Deciding native dependencies first keeps them out of the library.
2. A source with SQL or a wire protocol, such as an RDBMS, data warehouse,
   OLAP engine, or external Query Engine, uses ADBC access.
3. A source that does not fit ADBC's model uses direct access. Files and table
   formats (Parquet, CSV, Iceberg, Delta Lake), SaaS and REST APIs, and streams
   such as Kafka belong here.

ADBC is used where it fits because existing drivers already provide
connection, authentication, query submission, Arrow results, and cancellation,
leaving only source knowledge (catalog reading, dialect, and type
correspondence) to the connector. Files and table formats do not fit ADBC at
all: there is no server to connect to, the catalog comes from file listings or
table-format metadata, skipping by partitions and statistics is planned in
process, and Arrow reader crates already exist. SaaS and REST APIs and streams
lack the SQL and catalog model ADBC assumes, and their reads are paged or
unbounded.

In either form, ADBC drivers and reader crates only carry the source's data;
they never decide types. The connector converts the Arrow types and values they
return into Analytics values according to the
[type mapping](#type-mappings-and-coverage). The connector is the only
interpreter of the source, as recorded in
[Make one connector interpret each data source](design/make-one-connector-interpret-each-source.md).

## Calling conventions

This section defines the conventions shared by both capabilities. Rules
specific to each capability are in [Metadata retrieval](#metadata-retrieval)
and [Data read](#data-read), and the concrete API shape is in
[Connector API](#connector-api).

### Units and concurrency

Callers use the following units of the connector API. Functions that open a
data source, a session, or an operation return a handle, and later functions
name their target by handle.

| Unit | Meaning |
| --- | --- |
| Library | Initialized once per process. Receives the maximum result batch size, the location of ADBC drivers, and logging settings. The application initializes and shuts it down. |
| Data source | Holds one data source's connection settings and credentials. One is opened per registered data source and reopened when connection settings change. |
| Session | One connection to that data source. For a direct-access source without connections, the unit that runs one operation at a time. Callers open as many as they want concurrent operations. |
| Operation | One planning preparation, metadata request, or read request. Owns cancellation and error retrieval. Metadata/read source resources are created at the first pull; preparation resources are created by `finish_prepare_planning`. |
| Result stream | The result of a metadata or read operation, as an Arrow `ArrowArrayStream`. It owns no resources and only pulls results from the operation's resources. A planning-preparation operation has no result stream. |

If a stream is released without ever being pulled, nothing was sent to the
source. If an operation is cancelled before its first pull, the first pull
returns `cancellation`.
If planning preparation is cancelled before or during
`finish_prepare_planning`, that function returns `cancellation` and no planning
snapshot is created.

A process has several callers, such as the DuckDB adapter and the Physical
Catalog. They receive the initialized library from the application and share
it; none of them initializes or shuts it down. Every initialization setting
has a single process-wide value, so per-caller initialization would need a rule
for conflicting settings. The library returns `invalid state` when asked to
initialize while already initialized; it does not count references to accept a
second initialization, because that would let callers decide when shutdown
happens and take shutdown ordering away from the application. A process loads
the library as one shared library and callers do not statically link private
copies, because each copy would be initialized separately and would create its
own settings and ADBC driver manager.

Concurrency follows these rules:

- The library and data sources may be called from several threads at once. A
  caller may open sessions on one data source concurrently, one per thread.
- A caller does not call one session, operation, or result stream from several
  threads at once. The exception is cancellation, which any thread may call
  ([Cancellation and timeouts](#cancellation-and-timeouts)).
- A session runs at most one operation at a time. Starting another operation on
  the same session before the previous result stream has been fully pulled or
  released returns `invalid state` for the new request and leaves the previous
  stream running. Waiting instead would block a caller that reads the previous
  stream on the same thread.
- A connector holds at most one source connection per session and has no
  connection pool. A caller that needs concurrency opens that many sessions
  and decides whether to reuse them.
- A caller releases children (sessions of a data source, operations of a
  session, result streams of an operation) before their parent. If the order
  is wrong, the connector still never touches freed memory, and calls on a
  child after its parent was released return `invalid state`.

### Connection settings and credentials

The application passes a registered data source's connection settings as
options when it opens the data source. The connector owns their meaning. It
describes the options it accepts in the library's
[static information](#static-information), and the application builds its
registration settings from that description without interpreting their
content. The connector validates the settings when it opens the data source;
invalid settings produce an `invalid settings` error naming the offending
option.

Credentials are handled as follows:

- The connector marks in its static information which accepted options carry
  secret values. The application hides those values from display and logs.
- Each secret value is passed as its own option, never embedded in a connection
  string, because a caller cannot tell which part of a connection string to
  hide.
- The connector keeps secret values only while the data source and the
  sessions using it are alive and erases them once all of them are released.
  Sessions keep them alive because cancellation may open a separate connection
  to the source ([Cancellation and timeouts](#cancellation-and-timeouts)).
- The connector passes secret values to its Data Source access: as options to the
  ADBC driver with ADBC access, which also keeps them for that period, or to
  the client it uses with direct access. In both forms it uses only drivers,
  clients, and reader crates that keep secret values out of logs and errors
  ([Data Source access rules](#data-source-access-rules)).
- The connector API has no function that returns a received secret value.
- To rotate credentials, the caller opens a new data source and closes the old
  one after the operations on it finish.

### Requests and results

A function that starts an operation validates the request and immediately
returns the operation handle. Metadata and read operations also return a result
stream; their query is sent to the source when its first batch is pulled.
Planning preparation instead calls Data Source access from
`finish_prepare_planning`. In both cases the caller already holds the operation
handle and can cancel from another thread while the source is still answering.

- Validation errors (`invalid request`, `invalid state`) are
  returned immediately by the function that starts the operation.
- Errors at the source are returned as pull failures, or by
  `finish_prepare_planning` for preparation. The caller retrieves
  their structured details the same way as failures in the middle of a stream
  ([Errors in the API](#errors-in-the-api)).
- A stream's schema is available before the source is queried. For data read
  it follows from the request and the expected schema
  ([Read request](#read-request), [Schema check](#schema-check)); for
  metadata retrieval it is fixed by this contract
  ([Functions](#functions)).

For metadata and reads, creating and executing are not separate steps because a
created-but-not-run state would add another set of release cases. Planning
preparation is the deliberate exception: its separate finish call is what makes
a blocking setup query cancellable through the already-returned handle.

Results are returned as an Arrow stream pulled in order. A caller may release
the stream before receiving every batch; release means the rest is not wanted,
and the connector stops the source query and returns its resources. A session
whose stream was released early accepts the next request.

An operation is complete only when its stream has been pulled to the end
without an error, or when planning preparation has finished successfully. A
caller never treats the result of an operation that ended
by cancellation, timeout, or error as complete. How much of a partial result is
valid is defined for
[metadata retrieval](#cancellation-errors-and-declarations-for-metadata) and
[data read](#incomplete-reads).

### Cancellation and timeouts

A caller can cancel any running operation from any thread, including while the
first pull or `finish_prepare_planning` waits for the source. On cancellation the connector promptly stops
the source query and its own reading. With ADBC access it uses the driver's
cancellation; if that does not stop the query at the source, the connector
uses the source's own means, such as a PostgreSQL cancel request or MySQL
`KILL QUERY`. With direct access the connector itself stops reads and source
calls in progress. The next pull of a cancelled stream, or the blocked planning
finish call, returns `cancellation`.

The caller specifies a timeout per request. It counts from sending the query to
the source (the first pull, or planning finish) until the stream has been pulled to the end or preparation completes. An
operation that exceeds it is stopped the same way as a cancellation and
returns `timeout`. The timeout does not count from starting the operation so
that an adapter that starts an operation while planning and reads it later does
not time out before reading.

A separate connection timeout is set by an option when opening a data source.
Opening a data source or a session connects before returning a handle (for
sources with connections) and cannot be cancelled, so that limit is what stops
it.

After a cancellation, timeout, or error, the connector decides whether the
session remains usable. A session whose source connection broke in the middle
is never usable again because the connection state is unknown. An unusable
session returns `invalid state` to the next request; the caller closes and
reopens it.

### Errors

Errors are returned with one of the following kinds. Each error says whether
it may be retried, meaning the same request sent again unchanged may succeed.
The retry flag in parentheses is the default; a connector may decide otherwise
for an individual error. For example, a connection failure caused by an
unresolvable host name is not retryable.

| Kind | Meaning |
| --- | --- |
| `invalid settings` | Connection or library settings are invalid (not retryable). |
| `connection` | The data source cannot be reached, or the connection broke (retryable). |
| `authentication` | Authentication failed (not retryable). |
| `permission` | There is no privilege to read a particular target (not retryable). |
| `invalid request` | The request's shape or range is invalid, including a request outside the capability declaration (not retryable). |
| `invalid state` | The call order, session state, or invalidation of a planned subplan prevents accepting the request ([Units and concurrency](#units-and-concurrency), [Cancellation and timeouts](#cancellation-and-timeouts)). Finish the running stream, replan an invalidated subplan, or reopen an unusable session. |
| `stale registration` | The table shape registered in the Physical Catalog no longer matches the source: the schema check found a type or nullability mismatch ([Schema check](#schema-check)), or a table or column the request references no longer exists. Re-registration is required (not retryable). |
| `data` | A value that cannot be carried exactly was encountered ([Values](#values)) (not retryable). |
| `cancellation` | The caller cancelled (not retryable). The caller cancelled, so the caller decides whether to resend. |
| `timeout` | The specified time was exceeded (retryable). |
| `resource` | The source or the connector ran out of a resource such as connections or memory (retryable). |
| `data-source` | The Data Source could not run the request for a reason not covered above, such as a serialization failure. The Connector decides retryability from the Data Source error. |
| `internal` | A defect in the connector, its ADBC driver, or a reader crate (not retryable). |

Whether an error fails the whole operation or is recorded per target is
defined for
[metadata retrieval](#cancellation-errors-and-declarations-for-metadata) and
[data read](#incomplete-reads).

Error messages and diagnostics never contain credentials or the secret parts
of connection strings. They also never contain values derived from user data,
such as filter literals. Messages returned by the data source can contain user
data, so they are returned separately from the connector's own message
([Errors in the API](#errors-in-the-api)). The same rule governs what a
connector may emit in logs, metrics, and diagnostics: never credentials, secret
option values, whole connection strings, literal and parameter values, row
data, or messages returned by the data source.

### Capability declaration

Before receiving requests, a connector declares which part of the two
capabilities it can serve. Callers fetch the capability declaration per
session, because the same connector may declare differently depending on the
connected source's version and settings.

The declaration contains:

- **Implementation:** the connector's name and version and, with ADBC access,
  the name and version of the ADBC driver it uses.
- **Data Source:** the connected system's product name and version, and the
  Connector that owns its behavior.
- **Metadata retrieval:** type mapping coverage, the constraints, identifiers,
  and schema versions it returns, and its fixed settings
  ([Cancellation, errors, and declarations for metadata](#cancellation-errors-and-declarations-for-metadata)).
- **Data read:** whether it returns a read point, read consistency
  ([Consistency](#consistency)), the accepted Substrait version, and optional
  pruning hints for planner rules that are certainly unsupported
  ([Planning and residual operators](#planning-and-residual-operators)).

The metadata declaration is authoritative for metadata requests. Data-read
pushdown support is plan- and context-dependent: a static declaration may
prevent an obviously futile attempt, but it never authorizes a planner rewrite.
The connector's decision for the concrete candidate and planning context is
authoritative. The implementation and fixed settings in the declaration are
recorded in the
[retrieval info](#retrieval-info-and-content-digest) of table metadata and in
[read provenance](#read-provenance).

### Compatibility and versions

The contract's version is the connector API library's version. It is a
`MAJOR.MINOR.PATCH` semantic version, and the shared library's SONAME contains
MAJOR. Type mappings have no version of their own; a change in how a source is
mapped appears as a change in the connector's version
([Type mappings and coverage](#type-mappings-and-coverage)) and also raises the
library MINOR (table below).

Compatibility between the library and its callers has two kinds:

- **Binary compatibility:** a caller built against an older C header can load
  a newer library and call its functions. This covers function names and
  arguments, structure layout, and handles.
- **Data compatibility:** a caller can correctly handle received data with the
  knowledge it was built with. This covers output columns, the set of
  Analytics types, the items of capability declarations and static
  information, error kinds, and the accepted Substrait plans.

They are separate because some changes break only one. Adding an Analytics
type keeps binary compatibility, since no function or structure changes; only
the set of types that may appear in an Arrow schema grows. Changing a
structure's layout breaks binary compatibility even when the data carried is
the same.

The version to raise follows from which compatibility a change breaks:

| Change | Binary | Data | Version |
| --- | --- | --- | --- |
| Add a function to the connector API | Keeps | Keeps | MINOR |
| Change an existing function's arguments or a structure's layout | Breaks | — | MAJOR |
| Add an optional output column, or an item to the capability declaration or static information | Keeps | Keeps; callers ignore unknown items | MINOR |
| Add an Analytics type (an extension-encoded type such as `inet`, unsigned integers, and so on) | Keeps | Keeps; callers register columns of unknown types as unsupported | MINOR |
| Add a Connector or connector | Keeps | Keeps; callers do not open Connectors they do not use | MINOR |
| Change a type mapping or a connector's coverage | Keeps | Keeps; returned Analytics types may change, but retrieval info identifies which connector version produced them ([Retrieval info and content digest](#retrieval-info-and-content-digest)) | MINOR |
| Add an error kind | Keeps | Keeps; callers handle unknown kinds by the retry flag alone ([Errors in the API](#errors-in-the-api)) | MINOR |
| Rename an output column or remove an item | Keeps | Breaks | MAJOR |
| Change the meaning of an existing Analytics type or remove a type | Keeps | Breaks | MAJOR |
| Change the accepted Substrait version | Keeps | Breaks | MAJOR |
| Fix behavior to match the contract | Keeps | Keeps | PATCH |

MINOR changes keep data compatibility only because callers follow the handling
noted in the table. These are therefore caller obligations:

- Ignore unknown items, such as JSON document fields and output columns.
- Register a column of an unknown Analytics type as an unsupported column
  ([Unsupported columns](#unsupported-columns)). Unsupported columns cannot be
  projected, so an adapter never receives an unknown type. Rebuilding the
  caller against the newer version and re-registering gives the column its
  Analytics type.
- Handle an unknown error kind by its retry flag alone.

Incompatible pairs stop at startup, before any type or data is exchanged:

- A library with a different MAJOR has a different SONAME, so the dynamic
  linker does not load it.
- Before use, a caller reads the library version from the static information.
  If its MINOR is lower than the MINOR of the C header the caller was built
  with, the caller does not use that library.

The library and its callers ship in the same application distribution. The
Physical Catalog and adapters are built against the same C header, so they
know the same set of Analytics types. These rules govern the library and its
callers within one distribution. Exchanging Analytics types between
distributions, such as between nodes of a data mesh, is outside this contract.

Adding a Connector requires only a library MINOR. It is completed by adding a
connector, the Connector's type mapping, and, with ADBC access, its ADBC
driver, without changing the C ABI or its callers (the application, adapters,
and the Physical Catalog). Data Source-specific information therefore travels only
as data in the static information, the capability declaration, and connection
options; no Data Source-specific function or item is added to the connector API. A
Connector that needs a new Analytics type or a new kind of request is also
that addition, and follows that row of the table.

The library records its version in the retrieval info of table metadata and in
read provenance.

### Calling-convention conformance tests

These tests check the calling conventions and the ADBC driver version check
([Data Source access rules](#data-source-access-rules)) for every connector, regardless
of Connector. Cancellation, timeout, and stream release are tested with both
metadata and read requests.

| Action | Expected result |
| --- | --- |
| Release a parent before its child, then call the child | `invalid state` |
| Start a second operation on a session before the first stream ends | The second request returns `invalid state` and the first stream continues |
| Cancel from another thread before the first pull | The first pull ends with `cancellation` and no query was sent to the source |
| Release a stream without pulling | Nothing was sent to the source |
| Cancel from another thread during a pull, including while the first pull waits for the source | The next pull ends with `cancellation` and the source query has stopped |
| Exceed the request timeout | Ends with `timeout` and the source query has stopped |
| Release a stream partway | The source query stops and the same session accepts the next request |
| Break the source connection during a pull | The session becomes unusable and the next request returns `invalid state` |
| With ADBC access, install an ADBC driver outside the supported version range and open a session | Opening the session returns `invalid settings` |
| Send a request beyond the capability declaration | Starting the operation returns `invalid request` |
| Send a read request whose plan is not valid Substrait | Starting the operation returns `invalid request` |
| Fail during a pull | Retrieving the operation's error yields its kind and retry flag |
| All of the above | Secret values and filter literals never appear in error messages, diagnostics, or logs |

## Metadata retrieval

A connector reads namespaces, tables, columns, Data Source types, and constraints
from the source's catalog and decides an Analytics type for each column.

This section does not define:

- the stability of Physical Catalog resource identifiers, versioning of stored
  table metadata, change notification, or delivery to the Semantic Catalog;
  these belong to the Physical Catalog's own input and reference contracts;
- reading rows, which is [Data read](#data-read) and
  [Read request representation](#read-request-representation);
- how the connector SDK and connectors are built, which is
  [Connector internals](connector-internals.md) and
  [Connector implementation conventions](connector-implementation.md); or
- which data sources the product allows to be registered.

### Metadata requests

The name filters of the namespace list and the table list are sets of names. A
target is returned only if its name exactly equals one of them. Names are
compared as the source reports them, without case folding
([Table metadata](#table-metadata)). Patterns and wildcards are not accepted.
When a source cannot filter by name, the connector lists and then filters, so
every connector accepts filters.

Patterns are not accepted because escaping, case handling, and wildcard
semantics differ by source and cannot be given one meaning. The Physical
Catalog filters by names a user has chosen and needs no patterns; a caller that
needs patterns can list without a filter and filter itself.

The table list can span namespaces. When a source cannot list several
namespaces in one query, the connector queries each namespace internally and
returns one result, so callers never need to call per namespace.

Outputs are streams pulled in order, and a caller need not receive everything
at once. There is no cursor to continue across calls; a caller that needs the
rest narrows the request and sends it again.

A list is not a snapshot of the source at one point in time; the source may
change while it is being pulled. The point in time of table metadata is given
by each table's retrieval time
([Retrieval info and content digest](#retrieval-info-and-content-digest)).

### Table metadata

The record a connector returns for one table is its table metadata. It holds
both what the source declares and the Analytics types the connector derived.
They are separate items in one record, and receivers treat them separately:
the source declaration is an observation of the source, while Analytics types
are a derived record of the connector's mapping.

Table metadata is a shared output used by both the Physical Catalog and the
Semantic Catalog. The Physical Catalog uses it to plan and run queries; the
Semantic Catalog uses it as material for meaning. Table metadata therefore
includes items the Physical Catalog itself does not use, such as unique indexes
and CHECK constraints. Both can refer to the same table metadata through its
retrieval info and content digest. How each receives table metadata and which
items it uses is defined by their own input and reference contracts.

Table metadata returns, per level:

| Level | Items |
| --- | --- |
| Data source | The retrieved range and the retrieval info |
| Catalog | Name; omitted for sources without catalogs |
| Namespace | Name; for files and table formats, the path hierarchy under the registered location or the table-format catalog's namespace |
| Table | Name, kind (table, view, external table, and other kinds the source distinguishes), source identifier and schema version ([Data Source identifiers and schema versions](#data-source-identifiers-and-schema-versions)), columns, and constraints |
| Column | Name, ordinal, nullability, Data Source type, Analytics type or unsupported ([Unsupported columns](#unsupported-columns)), and Data Source identifier |

Names are returned as the source spells them, without case folding. Ordinals
are the source's column order and have no gaps.

Table metadata items are limited to those that receivers use with the same
meaning regardless of Connector. An item without a user has no basis for
deciding which meaning is correct and cannot be checked by conformance tests.
For the same reason, an attribute many sources have does not become an item
until a receiver needs it, and there is no key-value item for Data Source-specific
attributes. When a receiver needs a source attribute, it is added as an
optional, typed, source-independent item (a MINOR change).

### Constraints

The returned constraints are primary keys, unique constraints and unique
indexes, foreign keys, and CHECK constraints. NOT NULL is returned as column
nullability.

| Constraint | Items |
| --- | --- |
| Primary key | Constraint name and its columns in order |
| Unique constraint or unique index | Name, columns in order, whether it is a constraint or an index, and, for a partial index such as PostgreSQL `UNIQUE (code) WHERE deleted_at IS NULL`, its condition in the source's notation |
| Foreign key | Constraint name, its columns, the referenced table, and the referenced columns; references stay within the same data source |
| CHECK constraint | Constraint name and expression in the source's notation, uninterpreted |

The connector does not interpret partial-index conditions or CHECK
expressions. Returning conditional uniqueness as unconditional uniqueness would
lead users to misidentify rows, so the condition always accompanies it. When a
source does not enforce a constraint, such as a declared-only constraint, the
connector returns that distinction. Non-unique indexes, defaults, and
generated-column expressions are not returned. For sources without constraints,
such as Parquet and CSV, the connector declares that it does not return
constraints, which does not mean the source has none.

### Data Source identifiers and schema versions

A source identifier is a value, separate from the name, that the source uses
to refer to the same object: a PostgreSQL table OID, a SQL Server `object_id`,
or an Iceberg table UUID and field ID.

A source schema version is a value held by the source that changes when a
table's schema changes, such as Iceberg's current schema ID or a Delta Lake
table version. It does not necessarily increase (Iceberg can return to an
earlier schema ID) and may change for reasons other than the schema (a Delta
Lake table version advances with data changes). Equal values therefore imply
an equal schema, but a changed value does not imply a changed schema.

A connector returns these only when the source provides them and they are
readable with the retrieval privileges. They are the source's own values, never
values synthesized by the connector. How they determine Physical Catalog
resource identifiers, and how renames, drops, and re-creations are
distinguished, belongs to the Physical Catalog's contracts.

### Retrieval info and content digest

The same table can receive different Analytics types from a different
connector, connector version, ADBC driver or driver version, or fixed
settings. A different type mapping appears as a different connector version
([Type mappings and coverage](#type-mappings-and-coverage)). With direct
access, reader crates are built into the connector and their versions are part
of the connector's version ([Data Source access rules](#data-source-access-rules)), so
the connector's name and version identify them. To trace the cause of a
difference, table metadata carries the following. The producing connector and
its version, the library version, the ADBC driver and its version, the fixed
settings, and the retrieval time are together called retrieval info.

| Item | Content |
| --- | --- |
| Implementation | The producing connector's name and version, and the library version ([Compatibility and versions](#compatibility-and-versions)) |
| ADBC driver | The driver's name and version ([Data Source access rules](#data-source-access-rules)); present only with ADBC access |
| Fixed settings | An identifier of the settings that affect how types and values are produced ([Data Source types](#data-source-types)) |
| Retrieval time | Per table, the time the connector read the source's catalog |
| Completeness | Whether the table's metadata is complete or some information could not be obtained ([Missing information](#missing-information)) |
| Content digest | A value computed deterministically from the content items defined below |

The content digest is used to decide whether content changed. Content here is
what table metadata states about its target: whether it exists, the source
declaration, the derived Analytics types, the target's identity at the source,
and which of these could not be obtained. The digest includes only these, and
nothing that can change while they stay the same; otherwise the digest would
change without a content change and could not be used for the decision.

The digest is computed per table:

- **Included:** the catalog, namespace, and table names, the table kind, the
  source identifier, columns, constraints, a table's "does not exist" or
  "could not be read" state with its reason, completeness and missing items
  (both judged without the schema version), and "not offered" records for
  constraints and source identifiers ([Missing information](#missing-information)).
- **Excluded:** retrieval info, the source schema version, and missing-item and
  "not offered" records about the schema version.

The schema version is excluded because, like a Delta Lake table version, it can
change without a schema change. It is not needed to decide whether content
changed: what it tells, that equal values mean an equal schema, the digest
already tells. Whenever an item is added, whether it enters the digest is
decided at the same time by whether it is content in the sense above.

Every implementation that produces table metadata computes the digest,
including interim implementations outside this repository. The Physical
Catalog and the Semantic Catalog only compare digests. The normalization is
defined by this contract because digests are compared across table metadata
with the same library MAJOR.MINOR (below), so equal content must give equal
values across implementations and their versions. Conformance test expectations
are compared with the same normalization:

- Columns are ordered by ordinal; constraints by kind, name, and columns.
- Names and Data Source types are used as the source spelled them, without case
  folding.
- Field names used in serialization are the output column names defined by the
  corresponding API function ([Functions](#functions)).
- The normalized record is serialized with RFC 8785 JSON canonicalization and
  hashed with SHA-256.

Because field names are output column names, renaming an output column changes
the digest even when the content is the same. Renaming is a change to an
existing function's output and raises the library MAJOR. When a MINOR release
adds an optional item, new connectors fill it, so the digest changes even if
the source did not. A digest difference is therefore read as a content change
only between table metadata whose retrieval info has the same library
MAJOR.MINOR. PATCH is not part of that range: a PATCH that fixes an output
defect may change digests without a source change and trigger one extra
re-evaluation, which is accepted, because splitting the range per PATCH would
lose change detection for every table on every library update. How table
metadata of different versions is compared belongs to the Physical Catalog's
and the Semantic Catalog's contracts. Excluding items added by a MINOR release
from the digest is rejected, because changes to those items would then be
undetectable until the next MAJOR.

If any retrieval info item other than the retrieval time changes, the result is
new table metadata even when the content is the same. How the Physical Catalog
versions stored table metadata belongs to its own contracts.

### Missing information

Metadata retrieval distinguishes information it could not obtain from
information that does not exist. If the Physical Catalog read "could not be
read" as "does not exist", missing privileges and transient failures would be
recorded as dropped columns or tables.

| Situation | Result |
| --- | --- |
| The table is known to exist but its content cannot be read for lack of privilege | The table is returned as "could not be read", with its name and reason and no columns |
| A table named in a table metadata request does not exist | The table is returned as "does not exist", with its name and no columns, only when the connector can confirm it does not exist; otherwise as "could not be read". For example, when a catalog omits tables without privilege (such as PostgreSQL `information_schema.tables`), absence cannot distinguish nonexistence from missing privilege. |
| Columns were read but constraints, source identifiers, the source schema version, or some Data Source types could not be | The table metadata is returned as incomplete, naming the missing items |
| The catalog silently omits columns without privilege (such as PostgreSQL `information_schema.columns`) and the connector read it in a way that cannot detect missing columns | The table metadata is returned as incomplete |
| A namespace or table list cannot be read | The unreadable range is returned and no targets in it |
| The operation ended partway | The rules in [Cancellation, errors, and declarations for metadata](#cancellation-errors-and-declarations-for-metadata) apply |

An item the connector declares it does not return is not a missing item. Table
metadata records it as "not offered", distinct from incomplete. Incomplete
table metadata asserts nothing about its missing items. The Physical Catalog
never infers a drop or a change from incomplete table metadata; a dropped table
is inferred only from "does not exist", never from "could not be read".

### Data Source types

- The connector reads the source's own catalog, such as `information_schema`
  or `pg_catalog`, and returns as the Data Source type exactly the value the type
  mapping names as its origin
  ([Type mappings and coverage](#type-mappings-and-coverage)). It does not go
  through a JDBC driver's reinterpretation.
- Settings of JDBC drivers, the ADBC driver the connector uses, and client
  libraries do not change the retrieved schema. The connector fixes and
  declares the settings that change how types and values are produced.
- Excluded namespaces, such as PostgreSQL `pg_toast` and `pg_temp_*`, are
  listed by the connector as its own rule.

### Type mappings and coverage

A type mapping is a table that defines, for one Connector, where Data Source types
are read from, in what form they are returned, and how they map to Analytics
types. Each row is a mapping rule. The origin and form are fixed because the
same type is spelled differently depending on how it is read (PostgreSQL
`information_schema` reports `character varying` while `pg_type` reports
`varchar`); without fixing them, rule matching and conformance comparisons
would vary by implementation. There is one type mapping per Connector, not
one per connector. The current mappings are in [Type mappings](type-mappings.md).

Type mappings have no version. The mapping is built into the connector, so the
connector's name and version determine which mapping produced given table
metadata or values. A connector raises its version and records the change in
its changelog whenever its mapping or coverage changes. Conformance tests
([Metadata conformance tests](#metadata-conformance-tests)) check that a
connector maps as its type mapping says.

A connector declares as its coverage the mapping rules it can satisfy. A rule
is satisfied when the connector, in a data read, can produce values of the
rule's Analytics type without changing the source's values. A type conversion
during a read is allowed only when it provably does not change values, such as
widening an integer. Matching types through value-changing conversions would be
the "cast the difference away" alternative rejected in
[Make one connector interpret each data source](design/make-one-connector-interpret-each-source.md).
Covered rules are mapped as the type mapping says, and a column that hits an
uncovered rule is unsupported. For one Connector, a connector never returns
an Analytics type different from the type mapping.

### Unsupported columns

A column whose Data Source type cannot be mapped to an Analytics type under the type
mapping and the connector's coverage is returned as an unsupported column
([Unsupported columns](analytics-types.md#unsupported-columns)). It has a name,
ordinal, nullability, and Data Source type, and no Analytics type. The connector
returns one reason:

| Reason | Meaning |
| --- | --- |
| Not mapped | The type mapping states that the type is not mapped |
| Not supported by the connector | The type mapping defines a mapping, but this connector cannot produce values of that Analytics type ([Type mappings and coverage](#type-mappings-and-coverage)) |
| Parameter out of range | The type is in the type mapping, but a parameter such as precision or unit exceeds the mappable range |

When the Physical Catalog does not know an Analytics type the connector
returned, because the library MINOR is newer than the one the Physical Catalog
was built with ([Compatibility and versions](#compatibility-and-versions)), the
Physical Catalog registers the column as unsupported with the reason "not
supported by the caller".

A connector never implicitly converts an unsupported column to a nearby type.

A covered column can still hold values that cannot be carried exactly; those
are handled by [Values](#values).

### Cancellation, errors, and declarations for metadata

Metadata requests follow the [cancellation and timeout](#cancellation-and-timeouts)
rules and the [error kinds](#errors). Errors reach as follows:

| Error kinds | Reach |
| --- | --- |
| `authentication`, `invalid request`, `invalid state` | The whole operation fails |
| `permission` | Recorded per target as "could not be read" ([Missing information](#missing-information)); the operation does not fail |
| `connection`, `cancellation`, `timeout`, `resource`, `data-source`, `internal` | Before the first result, the whole operation fails. Partway, the table metadata completed so far is returned and the whole operation is incomplete. |

An incomplete operation is indicated by the stream ending with an error
([Requests and results](#requests-and-results)). `stale registration` and `data`
do not occur in metadata retrieval.

For metadata retrieval, the capability declaration includes:

- the Connectors it handles and, per Connector, its type mapping coverage;
- the kinds of constraints it returns;
- whether it returns source identifiers and source schema versions; and
- the fixed settings that affect how types and values are produced.

### Metadata conformance tests

These tests check, independently of implementation, that a connector meets
this section:

- Each Connector has fixtures containing every mapping rule of its type
  mapping, unmapped types, constraints, targets without privilege, nonexistent
  tables, several namespaces, and names that differ only in case.
- For each fixture, the expected table metadata is defined except for
  retrieval info.
- For covered rules the connector returns the expected Analytics type, and for
  columns hitting uncovered rules it returns unsupported columns with the
  reason "not supported by the connector". Any other result fails.
- Names, ordinals, nullability, Data Source types, and the representation of
  missing information match the expectations regardless of the declaration.
- A nonexistent table is returned as "does not exist" for sources where the
  connector can confirm nonexistence, and as "could not be read" otherwise.
- Name filters return only exact matches, without case folding.
- Every extension-encoded type the connector returns is registered in the
  [Analytics type model](analytics-types.md#registered-extension-encoded-types).
  Returning an unregistered one fails.
- Constraints, source identifiers, and source schema versions match the
  expectations where the connector declares it returns them, and are recorded
  as "not offered" where it declares it does not.

Whether the declared coverage matches the types and values a read produces is
checked by the [data read conformance tests](#data-read-conformance-tests).
Cancellation and timeout are checked by the
[calling-convention conformance tests](#calling-convention-conformance-tests).

## Data read

A connector returns the rows of one table as an Arrow stream typed by
Analytics types. The read is performed by the same connector as metadata
retrieval, with the same type mapping and the same fixed settings.

### Callers of data read

Only Query Engine adapters send read requests. When another product component
needs to inspect data values, such as sample values or value distributions,
its query passes through a Query Engine and reaches the connector as per-table
read requests.

Limits on result rows, bytes, and time are imposed on the result side by the
product, not by the connector. Passing a result row limit to a connector as the
read request's limit would change results when the Query Engine aggregates, as
in `SELECT DISTINCT`. Connectors perform no per-user authorization; a connector
uses the single credential registered for the data source.

To support those limits, a connector:

- stops on timeout and cancellation ([Cancellation and timeouts](#cancellation-and-timeouts));
- stops when the stream is released ([Requests and results](#requests-and-results)); and
- returns the information needed to record the query as
  [read provenance](#read-provenance).

### Read request

A Query Engine adapter first makes a planning attempt. The attempt contains:

| Input | Meaning |
| --- | --- |
| Rule | The native planner pattern being negotiated: initially read, projection, or filter |
| Current state | The optional accepted planned subplan and its complete adapter context already stored in the connector scan; absent only for the base read |
| Candidate | A serialized Substrait plan for the remote work the adapter proposes |
| Residual policy | Whether the adapter can retain the corresponding native operator |
| Candidate adapter context | The complete normalized Query Engine/session facts and semantic requirements for the complete candidate plan, not only the current native pattern |

The SDK validates the candidate. The Data Source Session supplies current source
facts such as Data Source version, type attributes, collation, time zone, snapshot,
and security identity. The SDK binds both sets of facts into the effective
planning context and dispatches the rule to the connector. An accepted
candidate becomes a serializable `PlannedSubplan` containing the Substrait
plan, Analytics output schema, table references, and a non-secret validation
manifest containing component digests and an aggregate fingerprint.
The adapter stores its sanitized adapter context beside that state and supplies
it again at execution. Credentials, secret values, open connections, and
process-local pointers are not part of either state.

Before the base read attempt, the adapter completes planning preparation for
the registered table bindings and expected schemas. This timeout-bound,
cancellable Data Source operation hydrates a Data Source Session snapshot. Subsequent
`plan_pushdown` calls name that snapshot and perform no source I/O inside native
optimizer callbacks. The snapshot is immutable; a new binding or invalidation
requires a replacement snapshot and a new base read.

The manifest covers every semantic input to the decision: connector-contract
and Substrait versions; connector implementation identity and semantic-rules
revision; the candidate plan; output schema and table references; normalized
adapter context; and the connector fact snapshot. Its versioned deterministic
encoding preserves absent-versus-empty values. Its per-component digests allow
diagnostics to name changed components without storing their values. A change
to any input requires replanning. Deadline and cancellation state are execution
inputs and do not change the decision identity. The complete model is in
[Validation identity](plan-pushdown-with-query-engine-planners.md#validation-identity).

Execution accepts only a `PlannedSubplan`. Its Arrow stream must have exactly
the planned column names, order, Analytics types, and nullability. Row order is
unspecified unless a future accepted rule explicitly guarantees ordering.

### Relation to table metadata and unsupported columns

Each column's Analytics type in the stream equals the Analytics type the same
connector would return in that table's metadata at that time. A type that
differs between metadata retrieval and data read would contradict the
single-interpreter principle and is not allowed.

Unsupported columns never appear in a read stream. The expected schema is built
from the Physical Catalog's Analytics types and contains no unsupported
columns. A request that references an unsupported column in its projection or
filter fails during Query Engine planning
([Unsupported columns](analytics-types.md#unsupported-columns)) and never
reaches the connector. If one does reach it, the connector returns
`invalid request` for a reference outside the expected schema.

### Schema check

Before returning the first row, the connector compares, for the columns the
request references, the expected schema's Analytics types with the Analytics
types obtained by mapping the types the source reports now. Nullability is
compared too: a column that is NOT NULL in the expected schema but now nullable
at the source is a mismatch; a column that is nullable in the expected schema
but now NOT NULL at the source is not, and the output column stays nullable.
This is the schema check. On mismatch the connector returns
`stale registration` instead of casting. A table or column referenced by the
request that no longer exists at the source also returns
`stale registration`. The error details list, per mismatched column, its name,
the expected Analytics type and nullability, and the current Data Source type and
nullability.

Output nullability follows the expected schema, and nullability is compared,
because adapters convert according to the planned types including nullability
([Execution](analytics-types.md#execution)). Otherwise an adapter could not
convert output the connector accepted.

The types the source reports may come from type information attached to query
results, such as a PostgreSQL row description or MySQL column definitions.
When that information alone cannot identify the mapping rule (PostgreSQL
domains and enums, MySQL `ENUM`), the connector reads the catalog to complete
it. The schema check runs in the connector, rather than as a cast elsewhere,
for the reasons in
[Make one connector interpret each data source](design/make-one-connector-interpret-each-source.md).
An adapter that later receives a type different from the planned Analytics
type fails the query as a conversion failure. That failure happens after the
connector's check passed, so it is treated as a connector defect, not as
`stale registration`.

### Values

A connector produces values as the type mapping says. Values that cannot be
carried exactly, such as PostgreSQL `numeric` NaN, MySQL `0000-00-00`, or a
time outside the Analytics type's range, are a `data` error under any mapping
rule. They are never turned into NULL or a nearby value.

They are not turned into NULL because that would silently change the source's
value and could make a pushed predicate disagree with a native residual
operator. The error occurs only when the value reaches connector output; a
user can still read a projection or filter whose accepted plan does not return
that value.

Carrying such values as NULL may be considered as a contract change only when
the error is shown to obstruct use on real data, together with a way to make
condition results agree.

### Incomplete reads

When a read ends with an error or a cancellation partway through the stream,
each row returned so far has correct values. Those rows are only part of the
result, and the caller never treats them as complete. `authentication`,
`permission`, `invalid request`, `invalid state`, and
`stale registration` are returned before the first row. Other kinds can occur
before the first row or partway.

### Read provenance

A read operation records provenance separately from its Arrow schema:

| Item | Content |
| --- | --- |
| Implementation | The connector's name and version, the ADBC driver's name and version (ADBC access only), and the library version |
| Fixed settings | An identifier of the fixed settings used to produce values |
| Request | The normalized form of the received request and its digest |
| Executed query | The query sent to the source; with direct access, the calls made to the source, such as the table-format snapshot read or API calls. Parameter values are excluded. |
| Read start time | The time the connector sent the query to the source |
| Read point | A value showing which point-in-time state the source read, when the source can return one; otherwise its absence |

The normalized request contains filter literals, which are user data. Callers
never write provenance to logs.

The caller retrieves the final provenance from the operation after execution
ends, including after an error or cancellation that occurred after the source
query was sent. Before the source query starts, the retrieval function returns
`invalid state`; if no query was ever sent, no read provenance exists. Keeping
provenance on the operation allows the Arrow schema to be available before
source I/O while read start time and read point are recorded when they actually
become known.

Provenance is used to record queries that inspected data values. Without a read
point, the same request sent later is not guaranteed to return the same rows,
and that absence is recorded with the provenance. Adapters pass provenance on
to their callers without dropping it.

### Consistency

A read request reads one point-in-time state of the source when the source can
run it as one query. There is no consistency across read requests: two tables
read in one query may reflect different points in time.

For data read, the capability declaration includes:

- whether a read point can be returned;
- whether one read request reads a single point-in-time state of the source;
  and
- the Substrait contract version and any conservative pruning hints described
  in [Planning and residual operators](#planning-and-residual-operators).

### Data read conformance tests

These tests check that a connector meets this section and produces the types
and values it declared in metadata retrieval:

- Each Connector has fixtures with boundary values for every mapping rule
  (minimum, maximum, precision limit, time zones, NULL) and values that cannot
  be carried exactly.
- For covered rules, the connector returns the expected Arrow types and values.
  This substantiates the coverage declaration.
- Projecting a column with a value that cannot be carried exactly is a `data`
  error; not projecting it is not.
- Exact projection and filter outcomes return the same rows and schema as the
  native operators they replace. Retained filter outcomes may reduce source
  work but preserve the native filter and therefore cannot change results.
  Fixtures include non-normalized expression trees and values whose comparison
  meaning varies by column, such as trailing spaces, letter case, accents, and
  columns with different character sets.
- Reading after changing a source column to a type that differs from the
  expected schema is `stale registration`. So is reading after dropping a
  referenced column or table, and after making a column that is NOT NULL in the
  expected schema nullable at the source. Making a column that is nullable in
  the expected schema NOT NULL at the source is not a mismatch, and the output
  column stays nullable.
- Each error returns with its defined kind and retry flag.

Cancellation, timeout, retrieving errors mid-stream, and the absence of secrets
and literals are checked by the
[calling-convention conformance tests](#calling-convention-conformance-tests).

## Read request representation

This section defines the plan shared by a Query Engine adapter and a connector
while they negotiate pushdown. Substrait is the shared relation and expression
AST; the SDK does not convert it to a second request or expression tree. The
rationale is in
[Use Substrait as the shared pushdown AST](design/use-substrait-as-pushdown-ast.md).

### Requirement

The representation has one requirement: the Query Engine does not know the
Connector's SQL dialect. A Query Engine that wrote SQL in the Connector's SQL dialect would
hold data source knowledge, which the
[component responsibilities](architecture.md#product-context) give to the
connector alone: adapters hold no data source knowledge, and Query Engines
never access a data source directly. Changing the Query Engine would also
change the queries sent to the source, breaking the guarantee that a column
has the same type and value under any Query Engine.

### Substrait subset

A candidate is passed as a serialized Substrait plan. Substrait is a published
specification for expressing relational plans independently of any Query
Engine. The same validated plan is retained in `PlannedSubplan` and later
passed to connector execution.

An accepted plan must:

- be a valid plan in the Substrait version fixed for the library MAJOR
  ([Compatibility and versions](#compatibility-and-versions)), as judged by the
  official `substrait-validator`; and
- fit the shape below.

A plan that is malformed or internally inconsistent is rejected as
`invalid request`. A valid Substrait plan outside the shared contract subset is
also rejected as `invalid request`. A candidate inside the shared subset can still be
`Unsupported` by a connector for the concrete planning context; that is a
normal planning outcome, not an invalid request. Adapters check in their own
tests that the candidates they build pass common validation.

The only accepted shape is:

~~~text
Plan
 +- RelRoot (output column names)
     +- ReadRel
         +- namedTable: the table reference
         +- baseSchema: the expected Analytics schema encoded as Substrait types
         +- filter or best_effort_filter: optional expression tree
         +- projection: optional column-selection mask
~~~

The current SDK scaffold carries `namedTable` as an unresolved non-empty table
reference. It validates the path shape and expected schema shape, but does not
yet prove that the reference belongs to the open Data Source or registered
schema. Binding resolution must promote it to a non-secret opaque table binding
before this boundary fulfills the intended registration and ownership checks;
until then callers must not treat `TableReference` as such proof.

Details of the shape:

- `ReadRel.filter` represents a filter that may be reported `Exact` only when
  the connector guarantees its semantics. `ReadRel.best_effort_filter`
  represents an optimization for which the adapter retains its native filter;
  it can never authorize removal of that operator.
- Column references in the filter and projection are positions in the expected
  schema (`base_schema`).
- Column names in `root` and `base_schema` follow Substrait's depth-first order
  through `Struct` fields.
- Functions such as comparisons reference Substrait's standard extensions
  through the plan's extension declarations.

A currently accepted candidate reads exactly one named table. The initial
rules are read, projection, and filter. Limit, aggregation, sort, TopN, and join
have separate rule identities but remain `Unsupported` until common validation,
connector execution, and an adapter rewrite path exist for each rule.

The library pins one accepted Substrait version. Adapters build only the
candidate pattern their native planner can remove or retain safely; they do not
need to convert a whole SQL statement or native plan.

### Filters and types

The shared contract does not define a second filter language or require AND,
CNF, DNF, or another normal form. A filter remains a Substrait expression tree,
including nested expressions and disjunctions. Common validation checks its
structure, declared functions, types, and resource bounds; the connector then
decides whether that concrete expression has the required meaning for the
source and session.

For example, `Text` comparisons can depend on Data Source type, collation, character
set, and trailing-space behavior, while floating-point comparisons can depend
on NaN and signed-zero semantics. Those facts cannot be decided from the
operator and Analytics type alone. A connector reports `Exact` only when it can
execute the complete candidate with the same semantics as the native operator.
Otherwise it reports `Retained` when the adapter can keep that operator, or
`Unsupported`.

`Retained` is not permission to run an arbitrary approximate filter. The remote
best-effort filter must return a superset of the rows selected by the native
predicate under every fact change allowed during execution; it may produce
false positives but no false negatives. The native residual removes the false
positives. Each future operator rule must define its own retained-safety
condition before that rule can return `Retained`.

The expected schema and literals map Analytics types to Substrait types as
follows:

| Analytics type | Substrait type |
| --- | --- |
| `Boolean` | `boolean` |
| `Byte`, `SmallInt`, `Int`, `BigInt` | `i8`, `i16`, `i32`, `i64` |
| `Float`, `Double` | `fp32`, `fp64` |
| `Decimal(p, s)` | `decimal<p, s>` |
| `Text` | `string` |
| `Blob` | `binary` |
| `Date` | `date` |
| `Time` | `precision_time<6>` |
| `Timestamp` | `precision_timestamp<6>` |
| `TimestampTz` | `precision_timestamp_tz<6>` |
| `Duration` | `interval_day<6>` |
| `Interval` | `interval_compound<9>` |
| `Array(T)`, `Map(K, V)`, `Struct(...)` | `list<T>`, `map<K, V>`, `struct<...>` |
| Extension-encoded types | The user-defined type `logical`, defined once in the ScalarDB Analytics Substrait extension |

`logical` takes as type parameters the type name, its parameters, and the
Substrait type that carries the value (the type corresponding to the storage
type, called the carrier type here). The extension
declares `logical` as a structure with exactly one field of the carrier type. A
literal is a `Literal.UserDefined` in its `struct` form with the value in that
field. Adding an extension-encoded type does not change the extension.

Extension-encoded types travel as a Substrait user-defined type because
lowering them to their carrier type would make `Uuid` and `Text`
indistinguishable in the plan, so the connector could not validate the schema
or the concrete expression semantics.

Conditions with literals that Substrait types cannot express, such as dates
outside `date`'s range, are not put in the plan by the adapter and stay in the
Query Engine.

### Planning and residual operators

The adapter chooses candidate granularity from a native planner pattern it can
safely replace or retain and proposes the complete next remote plan. The
connector evaluates that whole validated candidate with the effective planning
context; it does not rewrite the candidate or return isolated accepted
conjuncts through the shared API. Data Source attributes used for that judgment come
from the Data Source Session and must be valid for the planned execution rather
than copied from an older Physical Catalog registration.

Each rule returns one of three outcomes:

- `Exact`: the connector guarantees the candidate semantics, so the adapter may
  remove the corresponding native operator;
- `Retained`: the remote operation is an optimization and the adapter must keep
  the native operator; or
- `Unsupported`: neither native nor remote planning state changes.

The planning attempt states whether retaining the native operator is possible
and supplies the current planned subplan with its complete adapter context. The
candidate context likewise describes the complete candidate rather than only
the current operator. The SDK validates the candidate as an operator-specific
transition from that state. It rejects `Retained` for an
exact-only attempt, and a `ReadRel.best_effort_filter` introduced by this
attempt can never remove the corresponding native filter. A best-effort filter
already present in the current state belongs to its earlier residual and does
not prevent an exact outcome for a different current operator. The connector
does not return an SDK-owned residual expression, and the SDK does not evaluate
arbitrary residual predicates over Arrow batches.

Static capabilities are optional conservative hints. They may say that an
operator or extension is certainly unavailable, but only the connector's
plan-aware result authorizes a rewrite. The complete ownership and lifecycle
model is in
[Plan pushdown with Query Engine planners](plan-pushdown-with-query-engine-planners.md).

## Connector API

This section maps the requests, capability declarations, errors, and
provenance above onto connector API functions and the shape of exchanged data.
The exact C function names and structures are published in the C header with
the first API implementation and versioned with the library.

### Format

- The connector API is a C ABI. Callers in other languages, such as the C++
  DuckDB adapter and the Java Physical Catalog, use it directly or through a
  per-language binding that wraps it.
- Result streams and tabular inputs, such as a set of tables, use the Arrow C
  data interface (`ArrowArrayStream`, `ArrowArray`, `ArrowSchema`).
- Read requests are serialized Substrait plans (protobuf bytes).
- Capability declarations and static information are versioned JSON documents
  whose required content is defined in their sections below.
- Errors use the structure in [Errors in the API](#errors-in-the-api).

### Functions

The connector API has these functions:

- **Library initialization and shutdown.** Initialization takes the maximum
  result batch size, the ADBC driver location
  ([Data Source access rules](#data-source-access-rules)), and log level and destination.
  Static information ([Static information](#static-information)) can be read
  even before initialization.
- **Open and close a data source.** Input: Connector, connection setting and
  credential options, and connection timeout.
- **Open and close a session.**
- **Fetch the capability declaration**, per session.
- **Start, finish, and release planning preparation**, for table bindings and expected
  schemas. `start_prepare_planning` validates the request, takes the session
  lease, and returns an operation handle without source I/O.
  `finish_prepare_planning` may block while it hydrates the facts and returns an
  opaque planning-snapshot identity. Another thread may use the ordinary
  operation-cancel function while finish is blocked. Closing or cancellation
  releases its source resources and never publishes a partial snapshot. A
  successful finish completes the operation, releases its source resources and
  session lease, and transfers the snapshot to session ownership.
  The adapter calls `release_planning_snapshot` when its native scan state is
  discarded; closing the session releases any remaining snapshots.
- **Plan pushdown.** A versioned request envelope carries one rule, Substrait
  candidate, the optional current planned state with its adapter context,
  residual policy, complete candidate adapter context, and planning-snapshot
  identity. The Data Source Session adds facts from that immutable snapshot. The
  result is `Exact`, `Retained`, or `Unsupported`;
  accepted results carry serialized planned state. Planning is a synchronous,
  source-I/O-free call over the named immutable snapshot; a missing fact
  prevents an exact result.
- **Start an operation.** Every one takes a request timeout, validates the
  request, and immediately returns the operation handle and its result stream;
  the query is sent at the first pull ([Requests and results](#requests-and-results)).
  - Namespace list: a set of namespace names ([Metadata requests](#metadata-requests)).
  - Table list: a set of namespace names and a set of table names.
  - Table metadata: a set of tables, as Arrow rows.
  - Read: connector-accepted serialized planned state.
- **Cancel an operation**, from any thread.
- **Retrieve an operation's error**, in the structure of
  [Errors in the API](#errors-in-the-api), after a pull ends with an error.
- **Retrieve final read provenance** from a completed read operation. It is
  operation-owned JSON and is separate from the Arrow schema because its read
  start time and read point do not exist when `get_schema` first succeeds.
- **Close an operation.**

At the language-neutral level, planning and read execution form this boundary:

~~~text
start_prepare_planning(session, table_bindings, expected_schemas, timeout)
    -> operation
finish_prepare_planning(operation)
    -> planning_snapshot | error

plan_pushdown(session, planning_snapshot, planning_attempt)
    -> Exact(serialized_planned_subplan)
     | Retained(serialized_planned_subplan)
     | Unsupported(reason)
     | error

start_read(session, serialized_planned_subplan,
           complete_adapter_context, timeout)
    -> operation + ArrowArrayStream
     | error
~~~

The first function changes no native Query Engine state; the adapter interprets
the outcome and performs that rewrite. The second accepts only state produced
by successful planning. `start_read` does not contact the source. Before the
first pull issues the data query, the connector refreshes any facts that are not
fixed by a session or snapshot guarantee and establishes a guard that keeps
decision-relevant semantics stable through remote evaluation. A connector that
cannot establish such a guard cannot have returned `Exact` for a candidate that
depends on the mutable fact. A mismatch makes that pull return
`invalid state` with a structured plan-invalidated reason; a schema or object
mismatch remains `stale registration`. The current execution does not invoke
the optimizer again, but its caller may retry the query and cause replanning.
The complete responsibility and control flow are in
[Plan pushdown with Query Engine planners](plan-pushdown-with-query-engine-planners.md).

The Arrow schemas of the three metadata requests' inputs and outputs are the
items of [Metadata retrieval](#metadata-retrieval) laid out as columns. Each
request kind has a fixed output schema,
so its stream can return the schema before the source is queried. Table
metadata output has one row per table, with columns and constraints as nested
columns.

Namespaces are returned as sequences of names with no fixed depth. For sources
with catalogs, the first element is the catalog.

Each table metadata row states whether its table is complete, incomplete,
"could not be read", or "does not exist" ([Missing information](#missing-information)).
An incomplete operation is indicated by the stream ending with an error; the
rows returned before it are completed table metadata.

Table metadata is one record per table. Fetching names, columns, types, and
constraints through separate functions would let names and types of one table
disagree if the source changed between the calls.

For sources without namespace and table hierarchies, such as files and object
storage, the path hierarchy under the registered location maps to namespaces
and files or table-format tables map to tables.

Read provenance is returned as versioned JSON by the operation provenance
function. Adapters pass it to the Query Engine or audit path alongside the
completed operation, independently of the Arrow stream schema.

### Capability declaration document

The capability declaration is returned per session as a versioned JSON
document, with the items listed in
[Capability declaration](#capability-declaration),
[Cancellation, errors, and declarations for metadata](#cancellation-errors-and-declarations-for-metadata),
[Consistency](#consistency), and
[Planning and residual operators](#planning-and-residual-operators). Language
bindings hand it to callers as a typed value.

### Errors in the API

Errors are returned as a structure with these items:

| Item | Content |
| --- | --- |
| Kind | One of the [error kinds](#errors) |
| Retryable | Whether the request may be retried |
| Message | Composed by the connector; contains no credentials, literals, or messages returned by the data source |
| Data Source error code and message | As returned by the data source. The message may contain user data, so callers keep it out of ordinary logs. |
| Plan invalidation details | For `invalid state` caused by a planned-subplan mismatch: a stable reason code (`source facts changed`, `connector semantics changed`, `contract or Substrait version changed`, `adapter context changed`, or `plan/schema/bindings changed`) and the names of changed identity components; never their secret or literal values |
| Stale registration details | Per mismatched column, its name, the expected Analytics type and nullability, and the current Data Source type and nullability ([Schema check](#schema-check)); for missing tables or columns, their names |

A failing function returns this structure. When a pull of a result stream fails,
including when the first pull fails because of a source error, the Arrow stream
can return only an error number and string, so the caller retrieves this
structure through the operation's error retrieval function. An operation's
error remains retrievable until the operation is closed.

### Static information

The library returns static information as a versioned JSON document, available
even before initialization:

- the library version ([Compatibility and versions](#compatibility-and-versions));
- for each built-in connector:
  - its name, version, and the Connectors it handles;
  - for each accepted connection option: its key, whether it holds a secret
    ([Connection settings and credentials](#connection-settings-and-credentials)),
    whether it is required, its value type, its default, and its description;
  - its form of Data Source access ([Connector API and Data Source access](#connector-api-and-data-source-access));
    and
  - with ADBC access, the ADBC driver name and the supported driver version
    range ([Data Source access rules](#data-source-access-rules)).

The application uses this information to build registration settings and to
decide which values to hide from display and logs. It holds no per-source
settings of its own, so adding a Connector does not change the application
([Compatibility and versions](#compatibility-and-versions)).

### Data Source access rules

Each connector chooses one form of Data Source access by the rules in
[Connector API and Data Source access](#connector-api-and-data-source-access) and states
it in its static information.

With ADBC access:

- Each connector selects one ADBC driver and states the driver versions it
  supports.
- When it opens its first session, the connector checks that the version the
  driver reports is within that range and otherwise returns
  `invalid settings`.
- ADBC drivers are artifacts separate from the library. The library loads them
  through the ADBC driver manager from the location received at
  initialization. Where drivers are placed and how they are distributed belongs
  to the product's distribution, not to this contract.
- The connector fixes the driver options that change how types and values are
  produced, and includes them in the declaration's fixed settings.
- The connector uses only drivers that keep secret values out of logs and
  errors and that provide a means of cancellation.
- The name and version of the driver used are recorded in retrieval info and
  read provenance, because a different driver version can return different
  types or values.
- Raising the driver version means rerunning that connector's metadata and
  data read conformance tests with the new version.

With direct access:

- The implementation is written in Rust without native dependencies and built
  into the library.
- The connector fixes the reader crate settings that change how types and
  values are produced, and includes them in the declaration's fixed settings.
- The implementation and the clients and reader crates it uses keep secret
  values out of logs and errors.
- The implementation can stop reads and source calls in progress on
  cancellation and timeout ([Cancellation and timeouts](#cancellation-and-timeouts)).
- Reader crate versions are pinned as dependencies of the connector crate.
  Changing one raises the connector version, and retrieval info and read
  provenance identify it through the connector's name and version.
- Raising a reader crate version means rerunning that connector's metadata and
  data read conformance tests with the new version.

## Support tiers

Three tiers of connector support are recognized. In every tier one connector
handles all access to a data source, and results do not change with the Query
Engine. The tiers differ in how much of the type rules in
[Data Source types](#data-source-types), [Type mappings and coverage](#type-mappings-and-coverage),
and [Unsupported columns](#unsupported-columns) they satisfy.

| Tier | What it is | Rules not met | Recurring problem |
| --- | --- | --- | --- |
| Data Source-specific | Reads the source's own catalog (for table formats and files, their own metadata) through an ADBC driver dedicated to the source or through direct access | None | None |
| Generic ODBC | Reaches the source through an ADBC driver that uses ODBC, adding per-source dialect tables and type mappings on top of the processing shared by SQL sources | For sources whose own catalog cannot be read through ODBC, Data Source types and metadata follow the ODBC driver's report | Dependence on a driver's report, confined to one connector rather than varying by Query Engine |
| Query Engine-backed | Connects to an external Query Engine such as Trino, Spark, or Databricks through its ADBC driver, and obtains in one connector the sources that engine supports | Data Source types are that engine's types, and the sources' own catalogs cannot be read | The same, with a single mapping from that engine's types to Analytics types |

When Data Source types come from an ODBC driver's report or an external Query
Engine's types, their origin differs from the origin in the existing source
kind's type mapping. Because there is one type mapping per Connector, these
cases define a type mapping as a separate Connector that includes the path,
such as SQL Server through ODBC or Trino.

The breadth of sources reached through ADBC access depends on the ADBC driver
ecosystem and the maturity of the shared SQL-family processing rather than on
the number of connectors. Where an ADBC driver exists, a connector reduces to
source knowledge: catalog reading, dialect, and type correspondence.

## Implementability

The contract must be implementable by connectors other than PostgreSQL and
MySQL.
[Applying the conventions to other sources](connector-implementation.md#applying-the-conventions-to-other-sources)
shows how Oracle, ScalarDB, generic ODBC, Trino, and Iceberg connectors meet
each item or stay within the contract by declaring what they do not offer.
From those cases, the contract does not presuppose that:

- requests run at the source as SQL; planning candidates arrive as Substrait,
  and the connector decides support and translation for the concrete context;
- a source has constraints, source identifiers, schema versions, or read
  points; each can be declared as not offered;
- namespaces have a particular depth; they are returned as name sequences;
- a source has PostgreSQL or MySQL type or comparison semantics; type mappings
  are per Connector, and comparison differences are absorbed inside the
  connector; or
- an ADBC driver exists for the source, or the source fits ADBC; a missing
  driver can be written, and sources that do not fit use direct access.
