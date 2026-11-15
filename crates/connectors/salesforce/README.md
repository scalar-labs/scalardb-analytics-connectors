# Salesforce connector

The Salesforce connector reads a Salesforce org through its REST API (direct
access), using the client in [`client`](client). It reads objects from
describeGlobal and their fields from describe, maps field types by its
[type mapping](#type-mapping), and reads records with one SOQL query per read.

## Data Source access

| Item | Value |
| --- | --- |
| Reader crates | `connector-salesforce-client` 0.1.0 in this repository, over `reqwest` 0.12.28 with `rustls` 0.23.46 and `ring` 0.17.14 |
| Connection options | `login_url`, `auth_flow`, `client_id`, `client_secret` (secret), `refresh_token` (secret), `username`, `private_key` (secret), and `jwt_audience` |
| Fixed session settings | None |
| Fixed driver options | REST API version `62.0` |

`auth_flow` selects the OAuth 2.0 flow, and each flow takes its own options:

| `auth_flow` | Options |
| --- | --- |
| `refresh_token` | `refresh_token`, and `client_secret` when the connected app requires the secret for the refresh token flow |
| `jwt_bearer` | `username` and `private_key` (a PEM RSA key), and `jwt_audience`, which defaults to `login_url` |

An option of the other flow is `invalid settings`. `login_url` defaults to
`https://login.salesforce.com`; a sandbox uses `https://test.salesforce.com`
or its My Domain URL. Opening a Data Source obtains an access token, bounded by
the connection timeout, so rejected credentials fail there. The sessions of a
Data Source share its token, which the client renews once when Salesforce
rejects it.

The REST API version decides how describe reports fields, so it is fixed
rather than a connection option.

Salesforce's error codes are classified as follows; HTTP statuses and
transport failures that no code names follow `src/error_handler.rs`.

| Salesforce error | Kind |
| --- | --- |
| The token endpoint refuses the grant | `authentication` |
| `INVALID_SESSION_ID` after the client renewed the token once | `authentication` |
| `INSUFFICIENT_ACCESS`, `INSUFFICIENT_ACCESS_OR_READONLY` | `permission denied` |
| `INVALID_TYPE`, `INVALID_FIELD`: the object or field is hidden or deleted | `not found` |
| `REQUEST_LIMIT_EXCEEDED`, HTTP 429 | `resource`, retryable |
| `MALFORMED_QUERY`, `INVALID_QUERY_FILTER_OPERATOR` | `internal` |
| `QUERY_TIMEOUT`, `INVALID_QUERY_LOCATOR`, HTTP 5xx | `data-source`, retryable |
| A connection that cannot be made or breaks | `connection`, retryable |

A read sends one SOQL query and follows its `nextRecordsUrl` pages. Whether
later pages of a query reflect changes made after the first page was returned
is not confirmed, so the connector does not claim that a read sees one
point-in-time state of the object
([Consistency](../../../docs/connector-contract.md#consistency)).

## Dependencies

| Dependency | Used for | Ships with the connector | Tested versions |
| --- | --- | --- | --- |
| `reqwest` (MIT OR Apache-2.0) | HTTP | Yes, built into the library | 0.12.28 |
| `rustls` (Apache-2.0 OR ISC OR MIT) and `webpki-roots` (CDLA-Permissive-2.0) | TLS and its root certificates | Yes, built into the library | 0.23.46, 1.0.12 |
| `ring` (Apache-2.0 AND ISC) | The TLS cryptography provider and JWT signing | Yes, built into the library | 0.17.14 |
| `tokio` (MIT) | The runtime the client's requests run on | Yes, built into the library | 1.54.1 |

`ring` compiles its vendored C and assembly into a self-contained object and is
accepted by name in `deny.toml`
([Isolating native dependencies](../../../docs/connector-implementation.md#isolating-native-dependencies)).
No other exception. No dependency is installed by the user.

## Source metadata

The source metadata of an object is the JSON document of `SalesforceObject`,
whose rustdoc, with that of `SalesforceField`, defines each field. It holds the
attributes of the object's describe result that state what the object and its
fields mean:

| Item | From |
| --- | --- |
| Object: `name`, `label`, `custom` | describe |
| Field: `name`, `label`, `field_type`, `soap_type`, `length`, `precision`, `scale`, `digits` | describe `fields`: `name`, `label`, `type`, `soapType`, `length`, `precision`, `scale`, `digits` |
| Field: `nillable`, `custom`, `calculated`, `case_sensitive`, `filterable`, `unique`, `external_id` | describe `fields` |
| Field: `reference_to`, `relationship_name`, `compound_field_name` | describe `fields`: `referenceTo`, `relationshipName`, `compoundFieldName` |

Fields are in describe order, which is the column order. Picklist values,
child relationships, record types, and URLs are not read.

Table metadata derives from the document, and no connection option changes
it:

- Every object is in the one namespace `sobjects`, because Salesforce objects
  have no hierarchy, and table references are `sobjects` and the object's API
  name. The table kind is `table`.
- A column is nullable when its field is `nillable`.
- The constraints are the ones Salesforce keeps. Salesforce does not name
  constraints, so each takes its field's name:
  - `Id` is the primary key;
  - a `unique` field is a unique constraint; and
  - a reference field that points to one object is a foreign key to that
    object's `Id`. A polymorphic reference, which can point to several
    objects, is not a foreign key; its targets stay in `reference_to`.
- Source identifiers are not offered: describe returns no durable ID for an
  object or a field.
- Field-level security removes fields from describe without saying so, so the
  connector cannot rule out hidden columns, and table metadata is always
  incomplete ([Missing information](../../../docs/connector-contract.md#missing-information)).
- describeGlobal and describe hide the objects the user may not see, and a
  hidden object cannot be told from one that does not exist. A name in
  `sobjects` that describe does not find (HTTP 404) is "not found"; one that
  describe refuses with `INSUFFICIENT_ACCESS` is "permission denied". Object
  names are unique without regard to case, and Salesforce resolves them so, so
  `contact`, which describes as `Contact`, "does not exist", and so does an
  object that cannot be queried and a reference outside `sobjects`.
- Table lists hold the objects describeGlobal returns that are queryable and
  not deprecated, ordered by name.

For example, a `Contact` with `Id`, `LastName`, `Email` (unique), `AccountId`
(a reference to `Account`), and `WhoId` (a reference to `Contact` or `Lead`)
has the primary key `Id`, the unique constraint `Email`, and the foreign key
`AccountId` to `sobjects.Account(Id)`; its document lists both reference
targets of `WhoId`. This example is the conformance fixture's mock org, not an
org read with API version 62.0.

The document's shape can change between connector versions, so a stored
document is compared only with one from the same connector version.

## Type mapping

Data Source types are the `type` of each field in the object's describe
result, followed by `(precision,scale)` for `double`, `currency`, and
`percent`, such as `double(18,2)`.

SOQL compares only a field with a literal, so every comparison below is
between a filterable field and a literal of the column's type. The
[Pushdown](#pushdown) section adds how null and Boolean operators are
handled.

| Data Source type | Analytics type | Salesforce's comparison | Runs remotely |
| --- | --- | --- | --- |
| `id`, `reference` | `Text` | Equates the 15- and 18-character forms of an ID | None |
| `string`, `textarea`, `picklist`, `combobox`, `email`, `phone`, `url`, `encryptedstring` | `Text` | Equality ignores case; ordering follows the user's locale | None |
| `multipicklist` | `Text`: the selected values joined by `;` | Equality compares the set of selected values | None |
| `boolean` | `Boolean` | The same equality; SOQL does not order Booleans | Equality and inequality |
| `int` | `Int` | The same | Equality and ordering |
| `long` | `BigInt` | The same | Equality and ordering |
| `double(p,s)`, `percent(p,s)` | `Decimal(p, s)` when p is 1 to 76; otherwise unsupported (parameter out of range) | The same | Equality and ordering |
| `currency(p,s)` | `Decimal(p, s)` when p is 1 to 76; otherwise unsupported (parameter out of range) | In a multi-currency org, compares after converting to the corporate currency | None |
| `date` | `Date` | The same | Equality and ordering, with a literal from 1700-01-01 to 4000-12-31 |
| `datetime` | `TimestampTz` | The same | Equality and ordering, with a literal in whole seconds from 1700-01-01 to 4000-12-31, because a SOQL date-time literal has no fraction of a second |
| `time` | `Time` | The same | None |

Other types are not mapped:

- `base64`: a query returns the URL of the content rather than the content;
- `address` and `location`: compounds of fields that describe also lists one
  by one;
- `anyType`: its values change type from row to row; and
- `complexvalue` and every type the table does not list.

`time` comparisons agree with the Analytics comparison but stay in the local
plan.

The comparison column and the null behavior in [Pushdown](#pushdown) state the
connector's understanding of Salesforce's behavior. They are not yet checked
against a real org; the fixture's mock org models them.

## Coverage

Version 0.1.0 covers every mapping rule in its [type mapping](#type-mapping)
except `double`, `percent`, and `currency` above 38 digits, which are
unsupported (mapping not implemented): the client decodes decimals into Arrow
`Decimal32`, `Decimal64`, and `Decimal128`, the width the Analytics decimal of
each precision takes, but not `Decimal256`. Salesforce number fields hold at
most 18 digits. A
value with more digits than its field's precision or scale, which a formula
field could return, cannot be carried exactly and is a `data` error when a
read returns it.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Filter | The conjuncts of the filter's top-level `AND` that combine comparisons and null tests with `AND` and `OR` only, where every comparison runs remotely by the [type mapping](#type-mapping) and every field is filterable. The other conjuncts stay in a local filter. `NOT` stays local: SOQL comparisons are false rather than NULL for a null field, which agrees with the offered filter under `AND` and `OR` but not under `NOT`. `!=` is sent as `(field != value AND field != null)`, because SOQL's `!=` is true for a null field. |
| Projection | It selects at least one plain field and no filter remains local below it. A field projected twice is selected once and repeated in the output. |

## Known limitations

None. A deleted object or field cannot be told from one hidden from the user,
so reading after either is deleted fails with `not found`, as the contract
requires of a source that hides what the credential may not read.

## Compatibility

Only combinations that passed this crate's tests are listed.

| Salesforce | REST API version | Platform |
| --- | --- | --- |
| The fixture's mock org | 62.0 | macOS arm64 |

No real org has been tested.

## Tests

`tests/conformance` runs the SDK's connector conformance suite with this
connector's fixture against a mock org that the test serves over HTTP on a
local port. It needs no container or network access:

```sh
nix develop --command cargo test -p connector-salesforce
```

The mock's model of Salesforce is in `tests/conformance/soql.rs`: text compares
without regard to case, `!=` is true for null, and every other comparison with
null is false.
