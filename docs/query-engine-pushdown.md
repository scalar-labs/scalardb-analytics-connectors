# Query-engine pushdown integration

## Purpose

Pushdown is a logical-plan transformation performed jointly by a Query Engine
planner and a connector. It is not a static property of a connector in
isolation. This document compares the planner contracts of the Query Engines
that the connector API is expected to support and records the constraints they
place on the shared pushdown model.

The comparison was reviewed on 2026-10-21 against these versions:

| Query Engine | Reviewed version |
| --- | --- |
| Apache Spark | 4.2.0 |
| Trino | 484 |
| Apache DataFusion | 55.2.0 |
| DuckDB | 1.5.6, with unreleased `main` behavior called out separately |

The versions are evidence for the design, not an automatically supported
version matrix. Each Query Engine integration must pin and test the versions it
actually ships.

## Shared model

A pushdown attempt begins with a Query Engine logical plan. The engine or its
adapter recognizes a replaceable operator pattern, expresses the remote
candidate in Substrait, asks the connector whether that candidate is valid for
the concrete source and session, and then rewrites the native plan.

~~~text
Query Engine native logical plan
        |
        | engine-specific rule matches a replaceable pattern
        v
Substrait candidate + source/session semantics
        |
        | connector validates and plans remote execution
        v
pushdown decision + planned remote subplan
        |
        | engine or adapter rewrites the native plan
        v
native residual plan over a connector scan
~~~

Two plan transformations are involved:

1. The connector extends or combines the remote Substrait subplan that it can
   execute.
2. The Query Engine or its adapter replaces native operators and retains any
   residual operators required for correctness.

The connector cannot safely rewrite Spark, Trino, DataFusion, or DuckDB native
plan nodes. Conversely, the Query Engine cannot determine source-specific
support from an operator name alone. Data type, collation, source version,
session settings, and the already-pushed child plan can all change the answer.

## Operator comparison

The following table describes standard connector or data-source integration
points. A custom optimizer extension can add paths that the standard source API
does not provide.

| Operator or pattern | Spark 4.2 | Trino 484 | DataFusion 55.2 | DuckDB 1.5.6 |
| --- | --- | --- | --- | --- |
| Projection | Required-column pruning; nested pruning may be partial | Expression projection with returned replacement expressions and assignments | Column-index projection passed to `TableProvider::scan` | Table-function column pruning |
| Filter | Per-predicate pushed and post-scan sets; predicates supplied to one call are conjunctive | Accepted constraint plus remaining domain and expression | Per-expression `Exact`, `Inexact`, or `Unsupported`; scan filters are conjunctive | Simple table filters and a C++ complex-filter callback; expressions left in the callback vector remain above the scan |
| Limit | Supported; the source reports whether it is partial | Supported; the result reports whether the limit is guaranteed | Passed to `TableProvider::scan` as a lower-bound hint on rows produced; not pushed with inexact filters | No table-function limit hook; requires a logical optimizer extension |
| Aggregation | Complete or partial; Spark performs final grouping for partial pushdown | Full replacement through `applyAggregation`; the aggregate set is accepted as a unit | No standard `TableProvider` aggregation callback | No table-function aggregation hook; requires a logical optimizer extension |
| Sort | No standalone source hook | No standalone source hook | Physical-plan `try_pushdown_sort` supports exact, inexact, and unsupported results | No table-function sort hook; requires a logical optimizer extension |
| TopN | Dedicated sort-plus-limit hook; may be partial | Dedicated sort-plus-limit hook with a guarantee flag | Must be formed from the applicable sort and fetch/limit planning paths | Requires a logical optimizer extension |
| Join | Dedicated hook since 4.1; both sides must be compatible with one source execution domain | Experimental `applyJoin`; replaces both inputs and the join with one table handle | No standard `TableProvider` join callback | No table-function join hook; requires a logical optimizer extension |

This matrix has three immediate consequences:

- `Sort` and `TopN` are different planner patterns. Spark and Trino expose
  TopN but not standalone sort pushdown.
- The word `projection` does not denote the same capability everywhere. Spark,
  DataFusion, and DuckDB primarily expose column pruning, while Trino can push
  projection expressions.
- A uniform list of supported operator names cannot determine effective
  pushdown. The engine must first have a rule that can preserve the query's
  semantics after replacement.

## Apache Spark

Spark performs data-source pushdown through mix-in interfaces on
`ScanBuilder`. Its documented order is sample, filter, aggregate, limit or
TopN, offset, and finally column pruning. Spark owns the native logical-plan
rewrite and stores accepted state in the builder and resulting scan.

Projection uses `SupportsPushDownRequiredColumns`. This is a pruning contract,
not a general computed-expression projection contract. A source may prune only
part of a nested schema, but the scan schema must describe what it actually
returns.

`SupportsPushDownV2Filters` receives predicates that Spark treats as ANDed.
The source returns predicates that still require post-scan evaluation. Spark
distinguishes predicates that are applied exactly, predicates that help the
source but still require post-scan evaluation, and predicates the source does
not apply. This conjunctive input is a Spark API property and must not become a
restriction on the shared connector expression model.

Limit and TopN each expose a partial-pushdown flag. For a partial result, Spark
retains the corresponding operator. Aggregation also has an explicit complete
versus partial contract. When aggregation is partial, the source may return
duplicate grouping keys and Spark performs a final aggregation. Spark does not
push an aggregate across a filter that still requires post-scan evaluation.

Join pushdown is available from Spark 4.1. Both scan builders must report that
they can execute in one underlying source domain. The callback receives join
type, required columns and aliases for both sides, and the join condition.

References:

- [Spark `ScanBuilder` and pushdown order](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/ScanBuilder.html)
- [Spark V2 filter pushdown](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/SupportsPushDownV2Filters.html)
- [Spark required-column pushdown](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/SupportsPushDownRequiredColumns.html)
- [Spark limit pushdown](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/SupportsPushDownLimit.html)
- [Spark aggregation pushdown](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/SupportsPushDownAggregates.html)
- [Spark TopN pushdown](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/SupportsPushDownTopN.html)
- [Spark join pushdown](https://spark.apache.org/docs/latest/api/java/org/apache/spark/sql/connector/read/SupportsPushDownJoin.html)

## Trino

Trino exposes operator-specific callbacks on `ConnectorMetadata`:
`applyProjection`, `applyFilter`, `applyLimit`, `applyAggregation`,
`applyTopN`, and experimental `applyJoin`. Each successful call returns a new
connector table handle that represents the derived relation. Trino owns the
optimizer rule and replaces the native plan with that handle.

The callbacks may be invoked repeatedly. A connector must return an empty
result when an invocation makes no change, even if it generally supports that
operator, or the optimizer can loop indefinitely.

Filter pushdown returns the domain and expression that Trino must still
evaluate. Projection pushdown returns replacement expressions and new column
assignments, so a connector can push some projection expressions while leaving
others in Trino. Limit and TopN return guarantee flags; when the connector uses
the information only as a hint, Trino retains the native operator.

Aggregation does not have a generic partial-result flag comparable to Spark's.
The documented callback accepts the aggregate list as a unit; if one requested
aggregate cannot be handled, the connector returns no result. Join pushdown
replaces both scans and the join with one table handle and must remap every
column exposed by both inputs. The join API is explicitly experimental.

Trino has no standalone `applySort` callback. Ordering pushdown at this
boundary is represented by TopN.

Reference:

- [Trino 484 `ConnectorMetadata`](https://github.com/trinodb/trino/blob/484/core/trino-spi/src/main/java/io/trino/spi/connector/ConnectorMetadata.java)

## Apache DataFusion

DataFusion's standard `TableProvider` scan boundary directly accepts a column
projection, a list of filters, and an optional limit. The filter list is
conjunctive. `supports_filters_pushdown` returns `Exact`, `Inexact`, or
`Unsupported` for each expression. DataFusion retains an inexact filter above
the scan.

The scan limit is not an exact row-count contract: the provider must produce at
least the requested number of rows but may produce more. DataFusion does not
push the limit when any pushed filter is inexact because the early limit could
otherwise leave too few qualifying rows. The documented logical order at the
scan boundary is filter, limit, then projection.

DataFusion 55.2 also has physical-plan pushdown protocols. In particular,
`ExecutionPlan::try_pushdown_sort` distinguishes exact, inexact, and
unsupported ordering. The physical filter-pushdown protocol traverses the tree
downward to gather candidates and upward to let nodes absorb filters or retain
them.

There is no standard `TableProvider` callback for replacing an aggregate or a
join with connector execution. An integration that supports those operators
must install a custom logical optimizer rule and, when necessary, a custom
physical planner or execution node. DataFusion exposes optimizer-rule and query
planner extension points for this purpose.

DataFusion has an official `datafusion-substrait` crate that converts logical
and physical plans to and from Substrait. It can reduce adapter code, but its
documentation explicitly states that Substrait does not cover every
DataFusion plan and expression. The adapter must therefore still reject or
retain unsupported native expressions.

References:

- [DataFusion `TableProvider`](https://docs.rs/datafusion/latest/datafusion/catalog/trait.TableProvider.html)
- [DataFusion filter dispositions](https://docs.rs/datafusion/latest/datafusion/logical_expr/enum.TableProviderFilterPushDown.html)
- [DataFusion `ExecutionPlan`](https://docs.rs/datafusion/latest/datafusion/physical_plan/trait.ExecutionPlan.html)
- [DataFusion Substrait integration](https://docs.rs/datafusion-substrait/latest/datafusion_substrait/)
- [DataFusion custom optimizer-rule example](https://github.com/apache/datafusion/blob/main/datafusion-examples/examples/query_planning/optimizer_rule.rs)

## DuckDB

DuckDB 1.5.6 table functions expose projection and filter pushdown. Projection
pushdown supplies the selected column indexes. Simple filter pushdown supplies
`TableFilter` values. The C++ `pushdown_complex_filter` callback receives
arbitrary DuckDB expressions; expressions left in its vector are applied as a
regular filter after the scan. The stable C table-function API exposes
projection pushdown but does not expose the equivalent complex-filter
callback, so an integration using these filter facilities depends on the
version-pinned C++ extension API.

The stable table-function contract has no callbacks for limit, aggregation,
sort, TopN, or join pushdown. DuckDB does provide `OptimizerExtension`, whose
pre- and post-optimization callbacks can modify a logical plan in place. Those
operators therefore require an adapter-owned optimizer rule that recognizes a
supported subtree and replaces it with connector execution.

DuckDB `main` contains a newer `RemotePushdownOptimizer`. It can identify a
query that belongs to one remote catalog, ask the catalog whether expressions,
table constructs, and query nodes are supported, and dispatch a structured
`QueryNode` to the remote catalog. This mechanism is absent from the 1.5.6
release and must be treated as an unreleased reference design until it appears
in a pinned DuckDB release.

The Substrait community extension can produce and consume broad DuckDB
Substrait plans. It is useful conversion infrastructure, but it does not by
itself provide the planner callback or residual-plan decision required for
connector pushdown.

References:

- [DuckDB C table-function API](https://duckdb.org/docs/current/clients/c/table_functions.html)
- [DuckDB 1.5.6 C++ `TableFunction`](https://github.com/duckdb/duckdb/blob/v1.5.6/src/include/duckdb/function/table_function.hpp)
- [DuckDB 1.5.6 `OptimizerExtension`](https://github.com/duckdb/duckdb/blob/v1.5.6/src/include/duckdb/optimizer/optimizer_extension.hpp)
- [Unreleased DuckDB `RemotePushdownOptimizer`](https://github.com/duckdb/duckdb/blob/main/src/include/duckdb/optimizer/remote_pushdown_optimizer.hpp)
- [DuckDB Substrait community extension](https://github.com/substrait-io/duckdb-substrait-extension)

## Constraints on the connector contract

### Keep operator rules planner-coupled

The common model should expose operator- or pattern-specific planning rules,
not claim that a connector supports an operator independently of a planner.
The Query Engine adapter decides which native pattern it can replace and what
must remain above the connector scan.

The model must include composite planner patterns. TopN cannot always be
reconstructed by independently accepting sort and limit because Spark and
Trino negotiate it atomically.

### Keep Substrait as the shared relational AST

Relation and expression trees should remain Substrait messages at the planner
and connector boundary. Repacking them into SDK-owned `ColumnRef`, `Condition`,
or similar trees would duplicate the AST, restrict future operators, and still
leave each connector responsible for source-specific semantic checks.

Adapters do not need to convert an entire Query Engine plan. They only need to
construct the candidate operator pattern recognized by their native pushdown
rule. DataFusion can reuse its official conversion crate. Spark, Trino, and the
stable DuckDB table-function path still require adapter-specific conversion at
the inspected boundaries.

### Make plan-aware decisions authoritative

A static capability declaration can help a planner avoid obviously impossible
attempts, but it cannot authorize a rewrite. The authoritative decision needs
the candidate plan and planning context, including at least:

- Analytics and source types;
- collation and string comparison semantics;
- time zone, decimal, and null-ordering semantics;
- source product and version;
- source execution domain, snapshot, and security context; and
- functions and Substrait extensions used by the candidate.

### Model residuals as planner state

The common semantic outcomes are:

- **Exact:** remote execution guarantees the candidate's semantics, so the
  adapter may remove the corresponding native operator.
- **Retained:** remote execution can use the candidate as an optimization hint,
  but the adapter must retain the native operator.
- **Unsupported:** the remote plan is unchanged.

The connector should not return an arbitrary residual expression tree for the
SDK to evaluate. Candidate granularity is chosen by the adapter according to
what its planner can retain or reconstruct. An OR expression may be one
candidate; a planner that supplies individual conjuncts may offer them
separately. The shared contract imposes neither CNF nor DNF normalization.

Substrait's `ReadRel.filter` represents an exact filter, while
`ReadRel.best_effort_filter` represents a filter that need not be fully
satisfied by the read. These fields can carry the exact-versus-retained
distinction without inventing another expression AST. The native residual
operator remains owned by the Query Engine.

Partial aggregation follows the same rule. A planner that can finalize an
aggregate constructs a Substrait candidate with the appropriate aggregation
phase and retains its final aggregation. The connector executes that partial
candidate exactly. A planner without a finalization rule does not offer the
partial candidate. Substrait aggregate bindings distinguish
`INITIAL_TO_INTERMEDIATE`, `INTERMEDIATE_TO_INTERMEDIATE`,
`INITIAL_TO_RESULT`, and `INTERMEDIATE_TO_RESULT` phases.

References:

- [Substrait `ReadRel` filter semantics](https://substrait.io/relations/logical_relations/)
- [Substrait aggregate phases](https://substrait.io/expressions/aggregate_functions/)

## Delivery scope

The first connector planning delivery supports projection and filter rules.
It must establish the plan and result model needed by later limit,
aggregation, sort, TopN, and join rules without claiming those rules are
implemented.

Future Query Engine adapters can expose only the rules their pinned planner API
can apply safely:

- Spark and Trino can use their native operator callbacks.
- DataFusion needs custom optimizer integration for aggregation and join.
- DuckDB 1.5.6 needs an optimizer extension beyond projection and filter.

This asymmetry is expected. Effective pushdown is the intersection of the
adapter's safe rewrite, the connector's execution support, and the concrete
plan and session semantics.
