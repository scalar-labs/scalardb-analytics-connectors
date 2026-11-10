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
| Planned read | The remote execution of an adopted `PlannedPushdown`: its Connector-generated query and remote output schema, possibly carried from another process ([Read request](#read-request)) | An Arrow stream with the remote output schema |

Both capabilities follow the [calling conventions](#calling-conventions).
How inputs and outputs cross the API is defined in
[Connector API](#connector-api).

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
| Library | Initialized once per process. Receives the location of ADBC drivers and logging settings. The application initializes and shuts it down. |
| Data source | Holds one data source's connection settings and credentials. One is opened per registered data source and reopened when connection settings change. |
| Session | One connection to that data source. For a direct-access source without connections, the unit that runs one operation at a time. Callers open as many as they want concurrent operations. |
| Operation | One metadata request or read request. Owns cancellation and error retrieval. Source resources are created at the first pull. |
| Result stream | The result of a metadata or read operation, as an Arrow `ArrowArrayStream`. It owns no resources and only pulls results from the operation's resources. |

If a stream is released without ever being pulled, nothing was sent to the
source. If an operation is cancelled before its first pull, the first pull
returns `cancellation`.

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
`plan_pushdown` may call Data Source access while resolving a validated plan.

- Validation errors (`invalid request`, `invalid state`) are
  returned immediately by the function that starts the operation.
- Errors at the source are returned as pull failures, or directly by
  `plan_pushdown` during resolution. The caller retrieves
  their structured details the same way as failures in the middle of a stream
  ([Errors in the API](#errors-in-the-api)).
- A stream's schema is available before the source is queried. For data read
  it follows from the request and the expected schema
  ([Read request](#read-request), [Schema check](#schema-check)); for
  metadata retrieval it is fixed by this contract
  ([Functions](#functions)).

For metadata and reads, creating and executing are not separate steps because a
created-but-not-run state would add another set of release cases.

Results are returned as an Arrow stream pulled in order. A caller may release
the stream before receiving every batch; release means the rest is not wanted,
and the connector stops the source query and returns its resources. A session
whose stream was released early accepts the next request.

An operation is complete only when its stream has been pulled to the end
without an error. A
caller never treats the result of an operation that ended
by cancellation, timeout, or error as complete. How much of a partial result is
valid is defined for
[metadata retrieval](#cancellation-and-errors-for-metadata) and
[data read](#incomplete-reads).

### Cancellation and timeouts

A caller can cancel any running operation from any thread, including while the
first pull waits for the source. On cancellation the connector promptly stops
the source query and its own reading. With ADBC access it uses the driver's
cancellation, and only drivers whose cancellation stops the query at the source
are used ([Data Source access rules](#data-source-access-rules)). With direct
access the connector itself stops reads and source calls in progress. The next pull of a cancelled stream returns `cancellation`.

The caller specifies a timeout per request. It counts from sending the query to
the source at the first pull until the stream has been pulled to the end. An
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
| `invalid request` | The request's shape or range is invalid (not retryable). |
| `invalid state` | The call order or session state prevents accepting the request ([Units and concurrency](#units-and-concurrency), [Cancellation and timeouts](#cancellation-and-timeouts)). Finish the running stream or reopen an unusable session. |
| `stale registration` | The table shape registered in the Physical Catalog no longer matches the source: the schema check found a type or nullability mismatch ([Schema check](#schema-check)), or a table or column the request references no longer exists. Re-registration is required (not retryable). |
| `data` | A value that cannot be carried exactly was encountered ([Values](#values)) (not retryable). |
| `cancellation` | The caller cancelled (not retryable). The caller cancelled, so the caller decides whether to resend. |
| `timeout` | The specified time was exceeded (retryable). |
| `resource` | The source or the connector ran out of a resource such as connections or memory (retryable). |
| `data-source` | The Data Source could not run the request for a reason not covered above, such as a serialization failure. The Connector decides retryability from the Data Source error. |
| `internal` | A defect in the connector, its ADBC driver, or a reader crate (not retryable). |

Whether an error fails the whole operation or is recorded per target is
defined for
[metadata retrieval](#cancellation-and-errors-for-metadata) and
[data read](#incomplete-reads).

Error messages and diagnostics never contain credentials or the secret parts
of connection strings. They also never contain values derived from user data,
such as filter literals. Messages returned by the data source can contain user
data, so they are returned separately from the connector's own message
([Errors in the API](#errors-in-the-api)). The same rule governs what a
connector may emit in logs, metrics, and diagnostics: never credentials, secret
option values, whole connection strings, literal and parameter values, row
data, or messages returned by the data source.

### Compatibility and versions

The library and its callers (the application, the Physical Catalog, and
adapters) ship in the same application distribution and are updated together.
They are built from the same C header and the same set of Analytics types, so a
caller never receives an Analytics type, error kind, output column, or item it
was not built to handle. This contract therefore defines no rules for a caller
paired with an older or newer library. If adapters are distributed separately
or rolling upgrades become a requirement, the direction of allowed version skew
will be decided then and rules added for it.

The library has a version. It names the release and is returned in the
[static information](#static-information). Type mappings have no version of
their own; a change in how a source is mapped appears as a change in the
connector's version ([Type mappings and coverage](#type-mappings-and-coverage)).

Exchanging Analytics types between distributions, such as between nodes of a
data mesh, is outside this contract.

Adding a Connector is completed by adding a connector, the Connector's type
mapping, and, with ADBC access, its ADBC driver, without changing the C ABI or
its callers. Data Source-specific information therefore travels only as data in
the static information and connection options; no Data Source-specific function
or item is added to the connector API.

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
| Send a read request whose plan is not valid Substrait | Starting the operation returns `invalid request` |
| Fail during a pull | Retrieving the operation's error yields its kind and retry flag |
| All of the above | Secret values and filter literals never appear in error messages, diagnostics, or logs |

## Metadata retrieval

A connector reads namespaces, tables, columns, Data Source types, and constraints
from the source's catalog and decides an Analytics type for each column.

This section does not define:

- the stability of Physical Catalog resource identifiers, how receivers
  persist and version the table metadata they store, change notification, or
  delivery to the Semantic Catalog; these belong to the Physical Catalog's own
  input and reference contracts. How stored metadata is compared with a newer
  retrieval is defined here
  ([Comparing table metadata](#comparing-table-metadata));
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
([Retrieval info and determinism](#retrieval-info-and-determinism)).

### Table metadata

The record a connector returns for one table is its table metadata. It holds
both what the source declares and the Analytics types the connector derived.
They are separate items in one record, and receivers treat them separately:
the source declaration is an observation of the source, while Analytics types
are a derived record of the connector's mapping.

Each table read is returned in two layers:

- **Source metadata** is what the connector read about the table, in a shape
  the connector defines, such as PostgreSQL `pg_catalog` facts. It is returned
  as a JSON document. The Semantic Catalog receives it as material for meaning,
  and an LLM interprets it, so it has no fixed schema across Connectors.
- **Table metadata** is the source-independent subset that this section
  defines. The connector derives it from the source metadata, so it states
  nothing that the source metadata does not. The Physical Catalog registers it
  to plan and run queries. It also includes items the Physical Catalog itself
  does not use, such as unique indexes and CHECK constraints, because the
  Semantic Catalog relies on them too.

How each receives these layers and which items it uses is defined by their own
input and reference contracts.

Table metadata returns, per level:

| Level | Items |
| --- | --- |
| Data source | The retrieved range and the retrieval info |
| Catalog | Name; omitted for sources without catalogs |
| Namespace | Name; for files and table formats, the path hierarchy under the registered location or the table-format catalog's namespace |
| Table | Name, kind (table, view, external table, and other kinds the source distinguishes), source identifier ([Data Source identifiers](#data-source-identifiers)), columns, and constraints |
| Column | Name, ordinal, nullability, Data Source type, Analytics type or unsupported ([Unsupported columns](#unsupported-columns)), and Data Source identifier |

Names are returned as the source spells them, without case folding. Ordinals
are the source's column order and have no gaps.

Table metadata items are limited to those that receivers use with the same
meaning regardless of Connector. An item without a user has no basis for
deciding which meaning is correct and cannot be checked by conformance tests.
For the same reason, an attribute many sources have does not become a table
metadata item until a receiver needs it, and table metadata has no key-value
item for Data Source-specific attributes; those belong to the source metadata.
When a receiver needs a source attribute with one meaning across Connectors,
it is added to table metadata as an optional, typed, source-independent item.

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
such as Parquet and CSV, the connector records constraints as "not offered"
([Missing information](#missing-information)), which does not mean the source
has none.

### Data Source identifiers

A source identifier is a value, separate from the name, that the source uses
to refer to the same object: a PostgreSQL table OID, a SQL Server `object_id`,
or an Iceberg table UUID and field ID.

A connector returns one only when the source provides it and it is readable
with the retrieval privileges. It is the source's own value, never a value
synthesized by the connector. Comparison uses it to tell a rename from a drop
and re-creation ([Comparing table metadata](#comparing-table-metadata)). How it
determines Physical Catalog resource identifiers belongs to the Physical
Catalog's contracts.

### Retrieval info and determinism

Table metadata and source metadata are deterministic: when the source's state
is the same, a connector of the same version returns the same content, in the
same order. Columns are ordered by ordinal, and constraints by kind, name, and
columns. Values that change without a change at the source, such as the
retrieval time, are not part of the content and are carried separately as
retrieval info.

Determinism lets a receiver detect changes by comparing the metadata it stored
with newly retrieved metadata. The library performs that comparison
([Comparing table metadata](#comparing-table-metadata)).

The same table can receive different Analytics types from a different
connector, connector version, or ADBC driver or driver version. A different
type mapping or a different fixed setting appears as a different connector
version ([Type mappings and coverage](#type-mappings-and-coverage),
[Data Source types](#data-source-types)). With direct access, reader crates are
built into the connector and their versions are part of the connector's
version ([Data Source access rules](#data-source-access-rules)), so the
connector's name and version identify them. To trace the cause of a
difference, table metadata carries retrieval info:

| Item | Content |
| --- | --- |
| Implementation | The producing connector's name and version, and the library version ([Compatibility and versions](#compatibility-and-versions)) |
| ADBC driver | The driver's name and version ([Data Source access rules](#data-source-access-rules)); present only with ADBC access |
| Retrieval time | Per table, the time the connector read the source's catalog |

How the Physical Catalog versions stored table metadata belongs to its own
contracts.

### Missing information

Metadata retrieval distinguishes information it could not obtain from
information that does not exist. If the Physical Catalog read "could not be
read" as "does not exist", missing privileges and transient failures would be
recorded as dropped columns or tables.

| Situation | Result |
| --- | --- |
| The table is known to exist but its content cannot be read for lack of privilege | The table is returned as "could not be read", with its name and reason and no columns |
| A table named in a table metadata request does not exist | The table is returned as "does not exist", with its name and no columns, only when the connector can confirm it does not exist; otherwise as "could not be read". For example, when a catalog omits tables without privilege (such as PostgreSQL `information_schema.tables`), absence cannot distinguish nonexistence from missing privilege. |
| Columns were read but constraints, source identifiers, or some Data Source types could not be | The table metadata is returned as incomplete, naming the missing items |
| The catalog silently omits columns without privilege (such as PostgreSQL `information_schema.columns`) and the connector read it in a way that cannot detect missing columns | The table metadata is returned as incomplete |
| A namespace or table list cannot be read | The unreadable range is returned and no targets in it |
| The operation ended partway | The rules in [Cancellation and errors for metadata](#cancellation-and-errors-for-metadata) apply |

An item the connector never returns for its Connector, such as constraints for
Parquet, is not a missing item. Table metadata records it as "not offered",
distinct from incomplete. Incomplete
table metadata asserts nothing about its missing items. The Physical Catalog
never infers a drop or a change from incomplete table metadata; a dropped table
is inferred only from "does not exist", never from "could not be read".

### Comparing table metadata

A receiver that needs to detect changes stores the metadata it based its
decisions on and later passes it, with a newly retrieved one, to the library's
comparison. Each receiver stores what it uses: the Physical Catalog stores
table metadata, and the Semantic Catalog stores source metadata. One set of
rules serves every receiver, so the same source change is reported as the same
change whoever asks. If each receiver compared on its own, differences in how
it matched items, which values it ignored, and how it read incomplete metadata
would turn one change into different changes.

Callers store metadata in the output format of the
[Connector API](#connector-api); this contract defines no other storage format,
digest, or schema version. Storing it and choosing which stored table pairs
with which new one belong to the caller. Neither comparison needs a data
source or session.

The two comparisons differ in what they depend on:

- **Table metadata** has one shape for every Connector, so its comparison
  depends on no Connector. It reports item by item what changed, as below.
- **Source metadata** has the shape its connector defines, so its comparison
  depends on that connector. Each side is a table's state with, when the table
  was read, its source metadata document. When either side does not exist or
  could not be read, only the states are returned, by the same rules as for
  table metadata. Otherwise the producing connector restores both documents to
  its own source metadata type; they are the same exactly when the restored
  values are equal, and what changed is reported by comparing the table
  metadata the connector derives from both. A connector that cannot restore a
  stored document, for example after it changed the document's shape, rejects
  the comparison as an `invalid request`.

Retrieval info, such as the retrieval time and the producer, is never part of
either comparison: it changes without a change at the source. A caller that
needs to know whether the producer changed compares the retrieval info itself.

Within a table metadata pair, items are matched as follows:

- Columns are matched by source identifier when both sides have one, and
  otherwise by name. A column with the same source identifier and a different
  name is a changed column whose name changed: a rename. A column with the same
  name and different source identifiers is one removed and one added column: a
  drop and re-creation.
- Constraints are matched by kind and name.
- The table's own name and source identifier are compared as items. Different
  source identifiers on both sides mean the table was dropped and re-created.

Table metadata comparison returns, per pair:

| Part | Result |
| --- | --- |
| Table state | The state of each side: complete, incomplete, "could not be read", or "does not exist" ([Missing information](#missing-information)) |
| Table | Whether the name, kind, and source identifier changed or are unknown |
| Columns | Each added, removed, changed, or unknown column; a changed column carries both versions and names each attribute that changed or is unknown: name, ordinal, nullability, Data Source type, Analytics type or unsupported reason, and source identifier |
| Constraints | Each added, removed, or changed constraint, with both versions of a changed one, or that the constraints are unknown |

Unchanged items are not listed.

Table metadata comparison never reports a change it cannot observe, following
[Missing information](#missing-information):

- When either side is "could not be read" or "does not exist", only the table
  states are returned. A drop is indicated only by "does not exist" on the new
  side.
- An item that is missing on either side, or "not offered" on only one side, is
  unknown, never added, removed, or changed. An item "not offered" on both sides
  is unchanged. When constraints are unknown, no constraint is listed.
- A column whose Data Source type is unread on either side has its Data Source
  type and Analytics type unknown; its other attributes are still compared.
- A column present on only one side is unknown rather than added or removed
  when the other side may have hidden columns, or when an unmatched column on
  the other side cannot be told apart by source identifier, because the two may
  be one renamed column.
- When either side may have hidden columns, an ordinal difference is unknown,
  because a hidden column shifts the ordinals after it.

Comparison accepts only table metadata in this library's output schema, and
only source metadata documents the producing connector can restore. Metadata
stored before a library update that changed either cannot be compared.
[Compatibility and versions](#compatibility-and-versions) defines no version
skew, and stored metadata is the one place where output outlives a library
update; rules for comparing across library versions will be added when a
caller must keep comparing across such an update.

### Data Source types

- The connector reads the source's own catalog, such as `information_schema`
  or `pg_catalog`, and returns as the Data Source type exactly the value the type
  mapping names as its origin
  ([Type mappings and coverage](#type-mappings-and-coverage)). It does not go
  through a JDBC driver's reinterpretation.
- Settings of JDBC drivers, the ADBC driver the connector uses, and client
  libraries do not change the retrieved schema. The connector fixes the
  settings that change how types and values are produced, and raises its
  version when it changes them.
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

A connector's coverage is the set of mapping rules it can satisfy, and it is
part of the connector's version. A rule
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
| Mapping not implemented | The type mapping defines a mapping, but this connector version does not implement it: its ADBC driver, reader crate, or own conversion cannot produce values of that Analytics type without changing them, so the rule is outside its coverage ([Type mappings and coverage](#type-mappings-and-coverage)). A later connector version may map it. |
| Parameter out of range | The type is in the type mapping, but a parameter such as precision or unit exceeds the mappable range |

A connector never implicitly converts an unsupported column to a nearby type.

A covered column can still hold values that cannot be carried exactly; those
are handled by [Values](#values).

### Cancellation and errors for metadata

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
  reason "mapping not implemented". Any other result fails.
- Names, ordinals, nullability, Data Source types, and the representation of
  missing information match the expectations.
- Retrieving the same fixture twice returns the same table metadata, apart from
  retrieval info, in the same order.
- A nonexistent table is returned as "does not exist" for sources where the
  connector can confirm nonexistence, and as "could not be read" otherwise.
- Name filters return only exact matches, without case folding.
- Every extension-encoded type the connector returns is registered in the
  [Analytics type model](analytics-types.md#registered-extension-encoded-types).
  Returning an unregistered one fails.
- Constraints and source identifiers match the expectations where the
  connector returns them, and are recorded as "not offered" where the
  expectations say the connector does not return them.

Whether the coverage in the expectations matches the types and values a read produces is
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

- stops on timeout and cancellation ([Cancellation and timeouts](#cancellation-and-timeouts)); and
- stops when the stream is released ([Requests and results](#requests-and-results)).

### Read request

A Query Engine adapter first offers a complete planning region. The request contains:

| Input | Meaning |
| --- | --- |
| Offered plan | A decoded Substrait `Plan` for one complete native region alternative selected by the Adapter |

The offered plan is the complete semantic input to Connector planning. The
request has no separate Query Engine or session-semantics side channel. An
Adapter must express required semantics in the Substrait plan or a declared
extension; if it cannot, it must not offer that native region.

The SDK produces a `LogicalPlan` as the result of validation and conversion.
The Connector then resolves opaque
`NamedTable` bindings into its own private planning metadata and obtains the
Data Source facts required by that complete region. The SDK applies the
Connector-supplied semantic rewrite rules. The private metadata exists only
during planning and does not cross the connector API. The result is a
remote Substrait plan, an optional local Substrait plan, and a Connector-owned
query of type `Q` for the remote plan. The session's planner and executor
share the concrete query type `Q`. A successful request becomes a
`PlannedPushdown`. It contains the plans, opaque bindings, and the remote
execution: the query together with the remote output schema. The local plan,
when present, consumes that remote output through
the reserved `ExtensionLeafRel(RemoteResult)` and must not independently read a
named table.
Credentials, secret values, open connections, and process-local pointers are
not part of the result.

The remote execution can run in another process, as when a Query Engine plans
on one node and reads on another. The planning process encodes it as opaque
bytes; the reading process restores them and starts the read on a session it
opened itself from its own registration settings. The bytes are valid only for
the library version that produced them, which holds because the library and its
callers are updated together
([Compatibility and versions](#compatibility-and-versions)). They carry no
credentials and no local plan. The library version and the Data Source are not
checked on restore; bytes that cannot be restored are `invalid request`.

Resolution happens after common validation, because the complete validated plan
determines which catalog and source facts are needed. Resolution may perform
timeout-bound, cancellable Data Source I/O during planning. A session may
prefetch or cache facts, but that is an implementation strategy rather than a
required planning-snapshot protocol.

Source attributes that planning relied on, such as collations, character sets,
the Data Source version, and session settings, are treated as unchanged when
the planned query runs. If they change between planning and execution, the
result is not guaranteed. The only check at execution is the
[schema check](#schema-check).

Common structural checks inspect the same `PlannedPushdown`; they do not create
a second planning-result type. Stable identity and persistence are not part of
the shared contract until a concrete consumer requires them; the only restore
is a remote execution carried to another process as described above. The
complete model is in [Boundary checks](pushdown-planning.md#boundary-checks).

Remote execution accepts the remote execution of the adopted `PlannedPushdown`. Its Arrow stream must have
exactly the remote plan's output column names, order, Analytics types, and
nullability. It executes the retained opaque query directly and does not
translate the remote plan again. Before adopting the result, the Adapter must be
able to map that output and lower the complete local plan when present. If it
cannot, it discards that proposal without changing native state. After adoption,
the Query Engine executes the local plan when present. Row order is unspecified
unless a future accepted rule explicitly guarantees ordering.

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
A user can still read a projection or filter whose remote plan does not return
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

### Consistency

A read request reads one point-in-time state of the source when the source can
run it as one query. There is no consistency across read requests: two tables
read in one query may reflect different points in time.

### Data read conformance tests

These tests check that a connector meets this section and produces the types
and values it returns in metadata retrieval:

- Each Connector has fixtures with boundary values for every mapping rule
  (minimum, maximum, precision limit, time zones, NULL) and values that cannot
  be carried exactly.
- For covered rules, the connector returns the expected Arrow types and values.
  This substantiates the coverage in the fixture expectations.
- Projecting a column with a value that cannot be carried exactly is a `data`
  error; not projecting it is not.
- A remote plan followed by its optional local plan returns the same rows and
  schema as the offered region. A local filter may allow the remote plan to
  reduce source work without changing results.
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
while they negotiate pushdown. Substrait is the language-neutral interchange
representation at that boundary. Connector planning uses a strictly typed
internal representation of the accepted Substrait semantic profile, then
converts its result back to Substrait. That internal representation is not part
of this contract. The rationale is in
[Separate interchange and planning representations](design/separate-interchange-and-planning-representations.md).

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

A requested plan is passed as a decoded Substrait `Plan`. Substrait is a published
specification for expressing relational plans independently of any Query
Engine. `PushdownRequest` carries the offered plan. `PlannedPushdown` carries
the resulting remote and optional local plans beside the Connector-owned
executable query.

The request is non-parameterized. The adapter resolves every native runtime
parameter to a Substrait literal before offering the plan. Dynamic-parameter
expressions and plan-level `parameter_bindings` are outside the accepted
profile. The rationale is in
[Resolve pushdown parameters before the connector](design/resolve-pushdown-parameters-before-the-connector.md).

Offered and remote plans use the same explicit logical-operator shape. Their
role differs by placement: the offered plan describes the requested region,
while the remote plan describes the operators selected for Data Source
execution.

A plan whose required shape or fields cannot be read is rejected as `invalid
request`. A plan outside the shared contract shape is also rejected as
`invalid request`. A requested plan inside the shared subset can still be
`Unsupported` by a connector for its current Data Source facts; that is a
normal planning outcome, not an invalid request. Adapters check in their own
tests that the plans they build have the expected shape.

The initial accepted offered shape is:

~~~text
Plan
 +- RelRoot (output column names)
     +- ProjectRel? (outermost when present)
         +- FilterRel?
             +- ReadRel
                 +- namedTable: the opaque table binding
                 +- baseSchema: the expected Analytics schema encoded as Substrait types
~~~

This is a logical, pre-pushdown plan. The operator order is fixed: filtering is
evaluated over the read schema, then projection is evaluated over the filter
output. The offered `ReadRel` is a bare scan and does not contain `filter`,
`best_effort_filter`, or `projection` fields.

The initial accepted remote shape is:

~~~text
Plan
 +- RelRoot (remote output column names)
     +- ProjectRel? (outermost when present)
         +- FilterRel?
             +- ReadRel
                 +- namedTable: the opaque table binding
                 +- baseSchema: the expected Analytics schema encoded as Substrait types
~~~

Shipped filtering and projection remain explicit `FilterRel` and `ProjectRel`
nodes. Substrait's `ReadRel.filter` and `ReadRel.projection` instead describe
operations already embedded inside a scan. The common profile intentionally
does not use those fields, because pushdown planning here means deciding
whether a logical operator belongs to the remote or local plan. Mixing the two
kinds of pushdown would obscure that placement and create two representations
of the same operation. The Connector's remote query generator translates the
complete remote tree into the Data Source-specific query.

The SDK validates `namedTable` as a non-empty opaque `TableBinding`. Only the
Connector interprets it. During Connector planning, the Connector must ensure
that the binding belongs to the current Data Source Session and resolve the
physical relation, columns, and source facts required by its rules and query
generator. Their representation and validation are Connector-private; failures
stop planning before a `PlannedPushdown` is constructed.

Details of the shape:

- Offered `FilterRel` and `ProjectRel` represent the logical work to preserve
  across the remote and local plans. The initial `ProjectRel` subset uses direct
  field selections and an emit mapping. Project hints and common advanced
  fields are outside the initial profile.
- Offered `ReadRel.filter`, `ReadRel.best_effort_filter`, and
  `ReadRel.projection` are outside the initial contract. Approximate filtering
  with an exact local filter is deferred.
- Column references in offered `FilterRel` and `ProjectRel` nodes are positions
  in the expected read schema (`base_schema`) for this fixed initial shape.
- Column names in `root` and `base_schema` follow Substrait's depth-first order
  through `Struct` fields.
- Functions such as comparisons reference Substrait's standard extensions
  through the plan's extension declarations.

A plan in the current common validation subset reads exactly one named table.
The offered plan expresses logical operators as `FilterRel` and `ProjectRel`.
Connector rewrite rules operate on the strictly typed `LogicalPlan`; generated
remote and local plans are checked by internal decomposition-contract
assertions rather than passed through the external-input validator. Converting
them back to Substrait preserves explicit `FilterRel` and `ProjectRel` nodes,
with the local operators above `RemoteResult`. Limit, aggregation, sort, TopN,
and join remain outside the common validation subset until their end-to-end
validation, execution, and adapter rewrite paths exist. Adding them does not
add operator-specific planner methods: they become additional shapes inside a
complete request.

The library pins the Rust Substrait protobuf dependency used to represent
`Plan`; request-shape validation does not impose a separate runtime policy on
the plan's version metadata. Adapters build only a complete region their native
planner can rewrite around safely; they do not need to convert a whole SQL
statement or native plan.

### Filters and types

The shared contract does not define a second filter language or require AND,
CNF, DNF, or another normal form. A filter remains a Substrait expression tree,
including nested expressions and disjunctions. The initial shared validation
profile resolves declared function anchors through the pinned comparison and
Boolean extension URIs, resolves direct base-schema field positions, typed
scalar literals, function result types, and arity into `LogicalExpression`, and
checks that a filter condition has a Boolean result. It does not independently
type-check a Query Engine-produced expression tree. Substrait expressions are
regenerated from the connector-owned model when a plan crosses the boundary.
A declared function outside the initial mapping remains structurally valid but
is marked as preventing pushdown, so the containing operator remains in the
local plan. SQL translation also reports `Unsupported` if such an expression is
presented in a remote plan. Connector rewrite rules use the metadata loaded for
the planning call to decide whether each concrete expression has the required
meaning for the Data Source and session before it remains in the remote plan.

For example, `Text` comparisons can depend on Data Source type, collation, character
set, and trailing-space behavior, while floating-point comparisons can depend
on NaN and signed-zero semantics. Those facts cannot be decided from the
operator and Analytics type alone. A Connector rewrite rule changes the plan
only when the composition of its remote and optional local plans preserves the
offered semantics. A best-effort remote filter must return a superset of the requested
rows, and the generated local plan removes false positives. Each rewrite rule
defines and tests its own semantic conditions.

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

The Adapter offers a complete native region it can safely rewrite around. It
may issue other complete requests for alternative regions or topologies.
Pushdown planning evaluates that whole offered `LogicalPlan`. The Connector resolves
required source facts and supplies
its finite, tested rewrite rules; the SDK applies those rules in sequence.
Every rule converts one complete remote/local plan into another. Planning
returns a remote plan, an optional local plan, and the directly executable
remote query. Data Source attributes used by the rule come from the Data Source
Session and must be valid for planned execution rather than copied from an
older Physical Catalog registration.

Rules are evaluated in deterministic configured order. The initial `QueryPlan`
reduces the remote plan to the base read and represents the offered filter and
projection over `RemoteResult` in the local plan. A rule that cannot move or
rewrite more work returns that plan unchanged; an error stops rule evaluation.

The Connector result is one of:

- `Planned` without a local plan: all work executes remotely;
- `Planned` with a local plan: the Query Engine executes that generated plan
  over the remote plan's output; or
- `Unsupported`: neither native nor remote planning state changes.

A `Planned` result remains a proposal until the Adapter verifies that it can
install the remote output and complete local plan. A failure to lower rejects
only that proposal. The Adapter may retain the original native plan or adopt a
result from another complete request; it never reports Query Engine lowering
limits to the Connector.

The remote plan need not be a structural subset of the offered plan. A rewrite
may introduce intermediate aggregates, such as remote `SUM` and `COUNT` with a
local division for partial `AVG` pushdown. The Connector owns the semantic
equivalence guarantee for each rule; the SDK does not prove it.

The Adapter owns native scan state, join shape, and local-plan conversion. If it wants
the optimizer to compare materially different regions or join topologies, it
issues multiple complete requests. The Connector does not combine those
requests. The SDK does not evaluate arbitrary residual predicates over Arrow
batches.

Only the connector's plan-aware result authorizes a rewrite. The complete
ownership and lifecycle model is in [Pushdown planning](pushdown-planning.md).

## Connector API

This section maps the requests and errors above onto connector API functions and the shape of exchanged data.
The exact C function names and structures are published in the C header
`crates/connector-api/include/scalardb_analytics_connector.h`, which is
generated from the library's declarations and versioned with the library.

### Format

- The connector API is a C ABI. Callers in other languages, such as the C++
  DuckDB adapter and the Java Physical Catalog, use it directly or through a
  per-language binding that wraps it.
- Result streams and tabular inputs, such as a set of tables, use the Arrow C
  data interface (`ArrowArrayStream`, `ArrowArray`, `ArrowSchema`).
- Read requests are serialized Substrait plans (protobuf bytes).
- Static information is a versioned JSON document whose required content is
  defined in its section below.
- Errors use the structure in [Errors in the API](#errors-in-the-api).

### Functions

The connector API has these functions:

- **Library initialization and shutdown.** Initialization takes the ADBC
  driver location
  ([Data Source access rules](#data-source-access-rules)), and log level and destination.
  Static information ([Static information](#static-information)) can be read
  even before initialization.
- **Open and close a data source.** Input: Connector, connection setting and
  credential options, and connection timeout.
- **Open and close a session.**
- **Plan pushdown.** A versioned request envelope carries one complete offered
  Substrait region. The Adapter constructs that
  region from a maximal native pattern it can rewrite around. After common
  validation, the Connector loads private metadata for the opaque table
  bindings and any Data Source facts required by the region. The standard SDK
  planner applies the Connector's tested remote/local rewrite rules, and the
  Connector constructs the remote executable query. The result is `Planned` or
  `Unsupported`; a planned result carries remote and optional local Substrait
  plans plus the executable query. Metadata loading follows the
  ordinary planning timeout and cancellation rules. A Connector may use a
  session-local cache, but no cache identity crosses the API boundary.
- **Start an operation.** Every one takes a request timeout, validates the
  request, and immediately returns the operation handle and its result stream;
  the query is sent at the first pull ([Requests and results](#requests-and-results)).
  - Namespace list: a set of namespace names ([Metadata requests](#metadata-requests)).
  - Table list: a set of namespace names and a set of table names.
  - Table metadata: a set of tables, as Arrow rows.
  - Read: the remote execution of an adopted `PlannedPushdown`, or one restored
    from bytes.
- **Encode and restore a remote execution** as opaque bytes for another process
  ([Read request](#read-request)).
- **Compare table metadata.** Input: two Arrow arrays of table metadata with
  the same length; the tables at the same position form a pair. Output: an
  Arrow array with one comparison result per pair
  ([Comparing table metadata](#comparing-table-metadata)). It needs no
  Connector, data source, or session. Arrays of different lengths, or rows not
  in the table metadata output schema, are an `invalid request`.
- **Compare source metadata.** Input: a Connector and two sides of one table,
  each its state with, when it was read, its source metadata document. Output:
  the states, and when both were read, whether they are the same and the
  comparison of the table metadata the connector derives from them. It needs
  no data source or session. A document the connector cannot restore is an
  `invalid request`.
- **Cancel an operation**, from any thread.
- **Retrieve an operation's error**, in the structure of
  [Errors in the API](#errors-in-the-api), after a pull ends with an error.
- **Close an operation.**

At the language-neutral level, planning and read execution form this boundary:

~~~text
plan_pushdown(session, pushdown_request)
    -> Planned(planned_pushdown)
     | Unsupported(reason)
     | error

encode_remote_execution(planned_pushdown) -> bytes
restore_remote_execution(session, bytes) -> remote_execution | error

start_read(session, remote_execution, timeout)
    -> operation + ArrowArrayStream
     | error
~~~

`plan_pushdown` changes no native Query Engine state; the adapter interprets
the outcome and performs that rewrite. `start_read` accepts only an adopted
planning result. Source attributes used in planning are treated as unchanged
([Read request](#read-request)), and a schema or object mismatch found by the
[schema check](#schema-check) is `stale registration`. The current execution does not invoke
the optimizer again, but its caller may retry the query and cause replanning.
The complete responsibility and control flow are in
[Pushdown planning](pushdown-planning.md).

The Arrow schemas of the three metadata requests' inputs and outputs are the
items of [Metadata retrieval](#metadata-retrieval) laid out as columns
([Metadata schemas](#metadata-schemas)). Each
request kind has a fixed output schema,
so its stream can return the schema before the source is queried. Table
metadata output has one row per table, with columns and constraints as nested
columns and the source metadata as a JSON text column. A table that does not
exist or could not be read has no source metadata. The table metadata
comparison output is likewise the items of
[Comparing table metadata](#comparing-table-metadata) laid out as columns, with
a fixed schema.

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

### Metadata schemas

The three metadata requests exchange these Arrow schemas. Every enumerated
column is UTF-8 text with the values listed; a value outside them is a newer
contract. A name path, such as a namespace or a table, is `list<utf8>` with
non-null names. A nullable column is null when its row's state gives it no
value.

**Inputs.** A name filter of the namespace list or the table list is either
absent, which matches every name, or a set of names, which matches exactly
those names and may be empty. The tables of a table metadata request are Arrow
rows with one non-null column `table` of type `list<utf8>`. A null table, a
null or empty name, or another schema is an `invalid request`. The C functions
pass a filter as a nullable pointer to an array of names and the tables as an
`ArrowArrayStream` that the library takes and releases.

**Namespace list.** One row per namespace or unreadable range.

| Column | Type | Null | Content |
| --- | --- | --- | --- |
| `state` | utf8 | no | `listed` or `unreadable` |
| `namespace` | name path | yes | The namespace, when `listed` |
| `scope` | name path | yes | The range that could not be read, when `unreadable` |
| `reason` | utf8 | yes | Why the range could not be read, when `unreadable` |

**Table list.** One row per table or unreadable range.

| Column | Type | Null | Content |
| --- | --- | --- | --- |
| `state` | utf8 | no | `listed` or `unreadable` |
| `table` | name path | yes | The table, when `listed` |
| `kind` | utf8 | yes | `table`, `view`, `external`, or `other`, when `listed` |
| `source_kind` | utf8 | yes | The kind in the source's own term, when `kind` is `other` |
| `scope` | name path | yes | The range that could not be read, when `unreadable` |
| `reason` | utf8 | yes | Why the range could not be read, when `unreadable` |

**Table metadata.** One row per requested table, in request order.

| Column | Type | Null | Content |
| --- | --- | --- | --- |
| `state` | utf8 | no | `complete`, `incomplete`, `could_not_be_read`, or `does_not_exist` |
| `table` | name path | no | The table |
| `reason` | utf8 | yes | Why the table could not be read, when `could_not_be_read` |
| `kind` | utf8 | yes | As in the table list |
| `source_kind` | utf8 | yes | As in the table list |
| `source_id` | reported utf8 | yes | The table's source identifier |
| `columns` | list of column | yes | The columns in ordinal order |
| `columns_may_be_hidden` | bool | yes | Whether the catalog may have omitted columns without privilege |
| `constraints` | reported list of constraint | yes | The constraints in kind, name, and columns order |
| `source_metadata` | utf8 | yes | The source metadata JSON document |
| `retrieval` | retrieval | no | Retrieval info |

`kind`, `source_id`, `columns`, `columns_may_be_hidden`, `constraints`, and
`source_metadata` are non-null exactly when the table was read (`complete` or
`incomplete`). The nested types are:

| Type | Fields |
| --- | --- |
| reported `T` | `state` (utf8, not null): `value`, `missing`, or `not_offered`; `value` (`T`): present when `value`; `reason` (utf8): why it is `missing` |
| column | `name` (utf8, not null), `ordinal` (uint32, not null), `nullable` (bool, not null), `source_id` (reported utf8, not null), `type` (column type, not null) |
| column type | `state` (utf8, not null): `mapped`, `unsupported`, or `unread`; `data_source_type` (utf8): present unless `unread`; `analytics_type` (binary): present when `mapped`; `unsupported_reason` (utf8): `not_mapped`, `mapping_not_implemented`, or `parameter_out_of_range` when `unsupported`; `unread_reason` (utf8): when `unread` |
| constraint | `name` (utf8, not null), `enforced` (bool, not null), `kind` (utf8, not null): `primary_key`, `unique`, `foreign_key`, or `check`; `columns` (name list): absent for `check`; `unique_kind` (utf8): `constraint` or `index` for `unique`; `condition` (utf8): a partial index's condition; `referenced_table` (name path) and `referenced_columns` (name list) for `foreign_key`; `expression` (utf8) for `check` |
| retrieval | `connector` (struct of `name` and `version`, not null), `adbc_driver` (struct of `name` and `version`): present only with ADBC access; `library_version` (utf8, not null); `read_at` (timestamp in microseconds, UTC): when the table was read |

A column's `analytics_type` is the encapsulated Arrow IPC schema message
(continuation marker, length, and flatbuffer, as an IPC stream begins) of a
schema with one field: the column's Analytics type in its
[Arrow encoding](analytics-types.md#arrow-encodings), including extension
metadata, with an empty name and nullable. The column's own name and
nullability are the other fields of the column. Analytics types are recursive,
so no fixed set of columns can hold them, and their Arrow encoding is already
the one representation receivers share; the Arrow libraries of every caller
language read this message, as Arrow Flight carries schemas the same way. A
text notation of Analytics types would be a second type language to define
and keep in step.

Retrieval info is repeated in each row rather than carried in the schema's
metadata, which would make the schema differ by connector.

### Errors in the API

Errors are returned as a structure with these items:

| Item | Content |
| --- | --- |
| Kind | One of the [error kinds](#errors) |
| Retryable | Whether the request may be retried |
| Message | Composed by the connector; contains no credentials, literals, or messages returned by the data source |
| Data Source error code and message | As returned by the data source. The message may contain user data, so callers keep it out of ordinary logs. |
| Diagnostics | Ordered entries of a key and a typed value (Boolean, integer, floating-point, UTF-8, or bytes) attached by the connector, such as a vendor code. Keys may repeat. They are details for people and restricted logs, kept out of ordinary logs like the Data Source message, and callers do not branch on them. |
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
  produced, and raises its version when it changes them.
- The connector uses only drivers that keep secret values out of logs and
  errors and whose cancellation stops the query at the source.
- The name and version of the driver used are recorded in retrieval info,
  because a different driver version can return different types or values.
- Raising the driver version means rerunning that connector's metadata and
  data read conformance tests with the new version.

With direct access:

- The implementation is written in Rust without native dependencies and built
  into the library.
- The connector fixes the reader crate settings that change how types and
  values are produced, and raises its version when it changes them.
- The implementation and the clients and reader crates it uses keep secret
  values out of logs and errors.
- The implementation can stop reads and source calls in progress on
  cancellation and timeout ([Cancellation and timeouts](#cancellation-and-timeouts)).
- Reader crate versions are pinned as dependencies of the connector crate.
  Changing one raises the connector version, and retrieval info identifies it
  through the connector's name and version.
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
each item or stay within the contract by recording what they do not offer.
From those cases, the contract does not presuppose that:

- requests run at the source as SQL; planning requests arrive as Substrait,
  and the connector decides support and translation from the plan and current
  Data Source facts;
- a source has constraints or source identifiers; each can be recorded as not
  offered;
- namespaces have a particular depth; they are returned as name sequences;
- a source has PostgreSQL or MySQL type or comparison semantics; type mappings
  are per Connector, and comparison differences are absorbed inside the
  connector; or
- an ADBC driver exists for the source, or the source fits ADBC; a missing
  driver can be written, and sources that do not fit use direct access.
