# DynamoDB connector

The DynamoDB connector reads Amazon DynamoDB through the AWS SDK for Rust
(direct access). It lists tables with `ListTables`, reads each table's keys
from `DescribeTable`, maps the key attributes' types by its
[type mapping](#type-mapping), and reads items with one sequential `Scan` per
read. DynamoDB declares the types of key attributes only, so a table's columns
are its key attributes and one `item` column that holds each whole item as
DynamoDB JSON.

## Data Source access

| Item | Value |
| --- | --- |
| Reader crates | `aws-sdk-dynamodb` 1.140.0, `aws-config` 1.13.3, and `aws-smithy-http-client` 1.5.3, over `hyper` 1.12.0 with `rustls` 0.23.46 and `ring` 0.17.14 |
| Connection options | `region`, `endpoint`, `access_key_id`, `secret_access_key` (secret), and `session_token` (secret) |
| Fixed session settings | None |
| Fixed driver options | SDK behavior version `2026-11-17`; the standard retry mode of three attempts; eventually consistent reads |

`region` is required: requests are signed for it even when `endpoint` sends
them elsewhere, such as to a VPC endpoint or DynamoDB Local. `region` must be
a Region name of letters, digits, and `-`, and `endpoint` an HTTP(S) URL with
a host. Without `endpoint`, requests go to the Region's DynamoDB endpoint: an
endpoint URL that the environment (`AWS_ENDPOINT_URL`,
`AWS_ENDPOINT_URL_DYNAMODB`) or a shared config profile sets is not followed,
so the options alone name the Data Source.

`access_key_id` and `secret_access_key` give static credentials, with
`session_token` for temporary ones; giving only some of them is
`invalid settings`. Without them the AWS default credential chain supplies
credentials: environment variables, the shared config and credentials files,
web identity tokens, and the ECS and EC2 instance metadata endpoints. SSO
profiles and `credential_process` are not supported: the SDK features that
read them are not built, and the latter would start a process.

Opening a Data Source resolves credentials and sends one
`ListTables` request of at most one table, bounded by the connection timeout,
so missing or rejected credentials and an unreachable endpoint fail there.
Credentials that may not list tables still open, since they may read the
tables they are granted. The connection timeout also bounds establishing each
connection, and the SDK's behavior version bounds the wait for each response
to five minutes; a request that exceeds either is a retryable `connection`
error.

The SDK's standard retry mode makes up to three attempts of a request that is
throttled, fails with a server error, or cannot reach DynamoDB, backing off
between attempts. The errors that remain are classified as follows; HTTP
statuses that no code names follow `src/error_handler.rs`.

| DynamoDB error | Kind |
| --- | --- |
| `UnrecognizedClientException`, `InvalidSignatureException`, `ExpiredTokenException`, a credential chain that supplies no credentials | `authentication` |
| `AccessDeniedException` | `permission denied` |
| `ResourceNotFoundException` | `does not exist` |
| `ValidationException`: the connector built an invalid request | `internal` |
| `ProvisionedThroughputExceededException`, `ThrottlingException`, `RequestLimitExceeded` | `resource`, retryable |
| `InternalServerError`, another HTTP 5xx | `data-source`, retryable |
| A connection that cannot be made, times out, or breaks | `connection`, retryable |

DynamoDB's message is returned for each code above except
`ValidationException`, whose message can quote the request's expressions.

A read is one `Scan` of one segment, following `LastEvaluatedKey` from page to
page, each page requested when the previous one has been pulled. Reads are
eventually consistent, at half the read capacity of strongly consistent ones.
A `Scan` is not a point-in-time view of the table either way, so the connector
does not claim that a read sees one state of the table
([Consistency](../../../docs/connector-contract.md#consistency)). A projection
reduces the data DynamoDB returns but not the read capacity a `Scan` consumes,
which follows the size of the items read. Cancelling a read, or its timeout,
drops the request in flight; a page DynamoDB has already started reading still
consumes its capacity.

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| `aws-sdk-dynamodb`, `aws-config`, `aws-credential-types`, and the `aws-smithy-*` and `aws-sigv4` crates (Apache-2.0) | Requests, signing, the credential chain, endpoints, and retries | Yes, built into the library | 1.140.0, 1.13.3, 1.3.1 |
| `hyper` (MIT), `http` (MIT OR Apache-2.0), and `hyper-rustls` (Apache-2.0 OR ISC OR MIT) | HTTP, and checking `endpoint` as the SDK parses it | Yes, built into the library | 1.12.0, 1.5.0, 0.27.10 |
| `rustls` (Apache-2.0 OR ISC OR MIT) and `rustls-native-certs` (Apache-2.0 OR ISC OR MIT) | TLS, with the operating system's root certificates | Yes, built into the library | 0.23.46, 0.8.5 |
| `ring` (Apache-2.0 AND ISC) | The TLS cryptography provider | Yes, built into the library | 0.17.14 |
| `tokio` (MIT) | The runtime the SDK's requests run on | Yes, built into the library | 1.54.1 |

Every crate's default features are off: they would build `aws-lc-sys`, which
needs a C toolchain and CMake. `ring` compiles its vendored C and assembly into
a self-contained object and is accepted by name in `deny.toml`
([Isolating native dependencies](../../../docs/connector-implementation.md#isolating-native-dependencies)).
Root certificates come from the operating system's trust store, as the AWS
SDKs do, so corporate CAs and VPC endpoints need no extra setting; on macOS
`rustls-native-certs` reads it through the Security framework the system
provides. No other exception. No dependency is installed by the user.

The SDK's versions are pinned in the workspace manifest, and its minimum
Rust version sets the workspace's `rust-version`.

## Source metadata

The source metadata of a table is the JSON document of `DynamoDbTable`, whose
rustdoc, with that of the types it holds, defines each field. The static
information publishes the document's JSON Schema, derived from these types,
with that rustdoc as its descriptions. It holds the parts of the table's
`DescribeTable` result that define the table:

| Item | From `DescribeTable` `Table` |
| --- | --- |
| `name`, `table_id`, `arn` | `TableName`, `TableId`, `TableArn` |
| `key_schema` | `KeySchema` |
| `attribute_definitions` | `AttributeDefinitions` |
| `global_secondary_indexes`, `local_secondary_indexes`: name, key schema, projection | `GlobalSecondaryIndexes`, `LocalSecondaryIndexes` |
| `billing_mode` | `BillingModeSummary.BillingMode` |
| `table_class` | `TableClassSummary.TableClass` |
| `stream` | `StreamSpecification` |

Item counts, sizes, the table and index statuses, creation time, and
provisioned or on-demand throughput are not read: they change without any
change to the table's definition, and DynamoDB refreshes item counts only
about every six hours, so reading them would make two reads of an unchanged
table differ.

Table metadata derives from the document, and no connection option changes
it:

- Every table is in the one namespace `default`, because DynamoDB has no level
  between an account's Region and its tables. Table references are `default`
  and the table's name, so they stay the same when `region` or `endpoint`
  changes. The table kind is `table`; secondary indexes are not tables.
- The columns are the partition key, then the sort key, typed from
  `attribute_definitions`, then `item`, which holds the whole item
  ([Type mapping](#type-mapping)), unless a key attribute is named `item`
  ([Known limitations](#known-limitations)). They are NOT NULL, since every
  item has its key attributes.
- The constraint is the primary key over the key columns, named after the table,
  since DynamoDB does not name it. Secondary indexes do not keep their keys
  unique, so they are no constraints; they stay in the source metadata.
- The table's source identifier is `table_id`, which a re-created table does
  not share; the ARN stays the same across re-creation. Columns have no
  identifier.
- `DescribeTable` hides no key attribute, so table metadata is complete.
- IAM checks a request before DynamoDB looks up its table, so a table that
  `DescribeTable` refuses with `AccessDeniedException` is "permission denied"
  when `ListTables` lists it, and "not found" otherwise.
  `ResourceNotFoundException`, returned only to credentials that may describe
  the table, is "does not exist", and so is a reference outside `default` or a
  name that cannot name a table.
- Table lists hold every table `ListTables` returns, across its pages. When the
  credentials may not list tables, the namespace is unreadable.

For example, a table `orders` keyed by `pk` (`S`) and `sk` (`B`), with a
global secondary index keyed by `customer`, has the columns `pk` (`Text`),
`sk` (`Blob`), and `item` (`Json`), and the primary key `orders (pk, sk)`; its
document lists the
index and the definition of `customer`. This example is the mock endpoint of
this crate's tests, not a table read from DynamoDB.

The document's shape can change between connector versions, so a stored
document is compared only with one from the same connector version.

## Type mapping

DynamoDB declares the types of key attributes only: their Data Source types
are the `AttributeType` of each key attribute in `DescribeTable`
`AttributeDefinitions`, `S`, `N`, or `B`. Every other attribute is read through
the `item` column, whose Data Source type is the connector's own spelling
`item`. A comparison of key attributes compares a column with a literal of its
type.

| Data Source type | Analytics type | DynamoDB's comparison | Runs remotely |
| --- | --- | --- | --- |
| `S` | `Text` | The same: by the bytes of the UTF-8 encoding | None |
| `B` | `Blob` | The same: by unsigned bytes | None |
| `item` | `Json` | None | None |

`N` is not mapped: a number holds up to 38 significant digits with an exponent
from -130 to +125, and no Analytics `Decimal(p, s)` holds every such value
([Type mappings and coverage](../../../docs/connector-contract.md#type-mappings-and-coverage)).
A table keyed by a number lists that key as an unsupported column; its values
stay readable in `item`.

`item` holds the whole item, key attributes included, as DynamoDB JSON: each
attribute value is an object with one member named by its type descriptor, as
DynamoDB's API writes it, such as `{"price":{"N":"1.50"},"tags":{"SS":["a"]}}`.
The encoding loses nothing: a number keeps its exact decimal text, a set stays
apart from a list, and binary is base64 under `B` or in `BS`. The members of
the item and of every map are in the byte order of their names, and the
elements of every set in the byte order of their text or bytes, since DynamoDB
keeps no order of either, so equal items have equal text; list elements keep
their order. An attribute value of a type the SDK does not know is a `data`
error.

Typed columns for attributes other than keys would need their types stated by
a connection option, such as a declared schema, or inferred by sampling items;
neither is a mapping rule of this version, and either would make a table's
columns depend on that option.

`S` and `B` comparisons agree with the Analytics comparison but stay in the
local plan; see [Pushdown](#pushdown).

## Coverage

Version 0.1.0 covers every mapping rule in its [type mapping](#type-mapping).
A key attribute whose value is of another type than its definition cannot be
carried and is a `data` error; DynamoDB does not store such items.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Filter | Never. A filter expression is applied after items are read, so it saves no read capacity, and it is false rather than NULL for a missing attribute. |
| Projection | It selects at least one plain column and no filter remains local below it. A projection of key columns becomes the `Scan`'s projection expression, with a placeholder for each attribute name, so whole items are not returned; one that selects `item` reads whole items without a projection expression. A column projected twice is read once and repeated in the output. |

## Known limitations

- A table whose partition or sort key attribute is named `item` has no column
  holding the whole item, since its key column has that name; only its key
  attributes can be read.

## Compatibility

Only combinations that passed this crate's tests are listed.

| DynamoDB | AWS SDK for Rust | Platform |
| --- | --- | --- |
| The tests' mock endpoint | `aws-sdk-dynamodb` 1.140.0, `aws-config` 1.13.3 | macOS arm64 |

Neither DynamoDB nor DynamoDB Local has been tested.

## Tests

`tests/mock` drives the connector through the SDK's runtime against a mock
endpoint that the test serves over HTTP on a local port with canned DynamoDB
responses, including the IAM denials, rejected credentials, and throttling
that DynamoDB Local does not produce. It needs no container or network access:

```sh
nix develop --command cargo test -p connector-dynamodb
```

The conformance suite does not run against this connector yet.
