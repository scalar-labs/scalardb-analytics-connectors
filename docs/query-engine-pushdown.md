# Query Engine pushdown adapters

## Adapter boundary

A Query Engine adapter bridges the engine's native planner and the common
Substrait planning contract. It offers one complete native region and receives
a remote/local semantic decomposition.

~~~text
PushdownRequest {
    offered_plan: Substrait Plan
}

PushdownResult::Planned {
    PlannedPushdown {
        remote_plan
        local_plan
    }
}
~~~

Read, projection, and filter normally appear together in the offered plan. The
request has no operator-specific rule field.

## Responsibilities

The Adapter:

- recognizes native regions and converts each complete semantic alternative to
  Substrait, including required declared extensions;
- does not offer a native region whose semantics cannot be represented by that
  Substrait plan;
- chooses engine-specific join boundaries and alternative join topologies;
- checks whether each returned local Substrait plan can be converted to native
  Query Engine operators;
- stores the planned remote state in its scan or table handle; and
- adopts only results whose remote output and complete local plan it can install.

The Adapter does not resolve Data Source identifiers, inspect source catalogs,
or construct SQL.

## Applying a result

No local plan means complete remote execution:

~~~text
remote_plan -> final result
local_plan  = None
~~~

A local plan means the Query Engine performs the second stage:

~~~text
remote_plan -> Arrow batches -> local_plan -> final result
~~~

The remote plan's output schema is the boundary schema consumed by the local
plan. The local Substrait plan represents that input with the reserved
`ExtensionLeafRel(RemoteResult)` type. The Adapter replaces that leaf with the
remote Arrow stream. There is no separate intermediate-schema field.

Every planned result is a proposal. Before changing native planning state, the
Adapter checks the remote output mapping, lowers the complete local plan when
one exists, and checks the resulting schema mapping. A lowering failure rejects
that proposal; it is not a Connector error. The Adapter may consider results
from other complete requests or retain the original Query Engine plan.

Adoption is atomic. A rejected proposal, `Unsupported`, or an error leaves the
native plan unchanged.

## Connector-generated local work

The local plan need not be a literal remainder of the offered plan. A Connector
rewrite can introduce intermediate expressions or aggregates. For example,
partial average pushdown can return remote sum/count columns and a local
division plan.

The Connector does not know which generated local plans a particular Adapter
can lower. If an Adapter cannot translate a returned local plan, it rejects
that proposal. This does not prevent another request from producing complete
pushdown with no local plan, or a different proposal that the Adapter can
install.

The initial internal subset includes `RemoteResult`, `Filter`, and
direct-selection `Project`. The initial `QueryPlan` uses that subset to keep
every filter and projection local while the remote plan performs only the base
read. Connector rules then transform that complete plan in sequence; returning
it unchanged is a no-op. The SDK converts the resulting local logical plan to
the Substrait relation nodes that the Adapter lowers.

## Alternative requests and joins

The Adapter owns native join boundaries, input ordering, and alternative
topologies. It may issue multiple complete requests, discard results it cannot
lower, and let its optimizer compare the remaining proposals. Each result
independently contains its remote and local plans; Connectors do not combine
fragments from separate requests.

The Connector decides whether one offered topology has a semantics-preserving
remote/local rewrite for the current Data Source facts.

## Optimizer-time Data Source I/O

Connector planning may query the Data Source during an optimizer callback.
Facts needed by a rewrite rule are known only after the offered plan has been
validated. Planning is bounded by a timeout that the library fixes for every
Connector; the caller passes no timeout and does not cancel planning. When the
timeout passes, the Connector stops its catalog and Data Source calls and
planning returns `timeout`. Adapters must not assume planning is a pure
in-memory capability lookup.

Prefetching and caching are optional Connector implementation strategies. No
`PlanningSnapshot` abstraction is required by the shared contract.

## Planning result and execution

`PlannedPushdown<Q>` contains the remote and optional local plans plus the
`RemoteExecution<Q>` that execution needs: the query of type `Q` and the
remote output schema. For a Connector, `Q` is the SDK's `PlannedRead`, which
carries the read table, the registered base schema, and the Connector-owned
query that the Connector's planner and executor share. It excludes
credentials, connections, and process-local handles.

At execution, the adopted pushdown's `RemoteExecution` is read through
`execute_planned` with `Connector::read`, either directly or after its bytes
are carried to another process
([Execution in another process](pushdown-planning.md#execution-in-another-process)).
The SDK runs the schema check, and the Connector's executor then sends its
typed query directly. SQL-family Connectors use `connector-sql::SqlExecutor`,
whose query type is `SqlRead` and which delegates to the shared ADBC query
path.
For an adopted proposal, the Adapter then runs the local plan, if present, over
the returned Arrow stream.

The SDK does not re-prove semantic equivalence at execution time. Each
Connector's finite rewrite rules and conformance tests own that guarantee.

## Expected Query Engine extension points

The shared request/result model does not imply that every Query Engine can
offer or reinstall the same region through its standard connector API.

| Query Engine | Standard integration boundary | Consequence for requests and proposals |
| --- | --- | --- |
| Spark | Data Source V2 `ScanBuilder` mix-ins negotiate filters, required columns, aggregates, limits, and TopN through operator-specific callbacks | The Adapter accumulates native callback state into complete requests. It may adopt complete pushdown or a local plan expressible at that boundary; arbitrary Connector-generated local work and join replacement require a Catalyst optimizer integration. |
| Trino | `ConnectorMetadata.applyFilter`, `applyProjection`, `applyAggregation`, and `applyJoin` replace table handles and return SPI-specific mappings or remaining expressions | A table handle can retain the adopted remote state. Results whose local plan cannot be represented by the corresponding application result are discarded. Calls are iterative and a no-change result must not create another handle. |
| DataFusion | `TableProvider::scan` receives projection, filters, and limit and returns an `ExecutionPlan` | Scan-local requests fit the standard provider boundary. Aggregation and join regions require a custom optimizer rule; proposals outside the installed rule's lowering support are discarded. |
| DuckDB | Table functions receive projected columns and pushed filters | Scan-local projection and filter requests fit this boundary. Aggregation, join, and arbitrary local-plan replacement require an optimizer extension; unlowerable proposals are discarded. |

The [DuckDB adapter](../adapters/duckdb/README.md) implements the DuckDB row's
scan-local boundary; the other rows are future Adapter integration
requirements. Relevant upstream contracts are [Spark Data Source V2](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/ScanBuilder.html),
[Trino `ConnectorMetadata`](https://github.com/trinodb/trino/blob/master/core/trino-spi/src/main/java/io/trino/spi/connector/ConnectorMetadata.java),
[DataFusion `TableProvider`](https://docs.rs/datafusion/latest/datafusion/catalog/trait.TableProvider.html),
and [DuckDB table functions](https://github.com/duckdb/duckdb/blob/main/src/include/duckdb/function/table_function.hpp).

## Implementing an Adapter

How an Adapter uses the connector API, builds offered plans, applies results,
maps types, and is tested is defined in
[Query Engine adapter implementation conventions](adapter-implementation.md).
