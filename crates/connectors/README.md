# Connectors

Each directory here is one connector crate. How a connector is built, what it
writes, and how it is tested are in
[Connector implementation conventions](../../docs/connector-implementation.md#building-a-connector).

| Connector | Data Source access |
| --- | --- |
| [postgresql](postgresql/README.md) | ADBC, through the Apache Arrow ADBC PostgreSQL driver |
| mysql | Not implemented; the crate is a placeholder |

## README template

Every connector's README records the same items, so that its coverage and
compatibility can be compared across connectors and checked against its
conformance fixture. Copy this outline and replace each placeholder; the
[PostgreSQL README](postgresql/README.md) is a filled example.

````markdown
# <Source> connector

<One paragraph: the Data Source, the form of Data Source access, the catalog
the connector reads, and a link to the source's section of
docs/type-mappings.md.>

## Data Source access

| Item | Value |
| --- | --- |
| ADBC driver | <Driver name and how it is located> (ADBC access) |
| Supported driver versions | <The range in the connector's `DriverRequirement`> (ADBC access) |
| Reader crates | <Crates and pinned versions> (direct access) |
| Connection options | <Keys, marking which are secret> |
| Fixed session settings | <Settings run right after connecting, or "None"> |
| Fixed driver options | <Driver options or reader settings that change how types and values are produced, or "None"> |

<Anything about the options or the driver a caller needs to predict results.>

## Source metadata

<What the source metadata document holds and the type whose rustdoc defines
its fields. A table of each item and the catalog, metadata, or reader output
it comes from. What is not read. How table metadata derives from the document,
such as nullability. One example table and its document, with the source
version it was read from. That the document's shape can change between
connector versions, and what that means for comparing stored source
metadata.>

## Coverage

<Which mapping rules of the type mapping this version covers. List each rule
the type mapping maps but the connector does not cover, with the reason. List
the values that cannot be carried exactly and are `data` errors.>

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| <Filter, projection, ...> | <The condition under which the source evaluates it with the same meaning> |

<What always stays in the local plan.>

## Known limitations

<Each known deviation stated in the conformance fixture: what the connector
does instead of what the contract requires, and where the cause lies, such as
the driver. "None" when the fixture states none.>

## Compatibility

Only combinations that passed this crate's tests are listed.

| <Source> | ADBC driver | Platform |
| --- | --- | --- |
| <Version> | <Version> | <Platform> |

## Tests

<What the tests need, such as a container runtime and the driver directory,
the command that runs them, and the variables that select another server
image.>
````
