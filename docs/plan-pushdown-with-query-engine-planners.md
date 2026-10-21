# Plan pushdown with Query Engine planners

## Purpose

Plan pushdown moves work from a Query Engine's native logical plan into a
connector-executed Substrait subplan without giving the connector ownership of
the native plan. The Query Engine adapter and connector make this transformation
together: the adapter identifies a replaceable native pattern, and the connector
decides whether that concrete pattern is safe and useful for the source and
session.

This boundary lets each Query Engine preserve its own optimizer semantics while
connectors make source-specific decisions that cannot be derived from a static
operator list. Data type, collation, source version, session settings, security
context, and the already-pushed child plan can all change the answer.

## Two plans with different owners

A pushdown attempt involves a native plan and a remote plan. They are related,
but neither side rewrites the other's representation.

~~~text
Query Engine native logical tree
        |
        | adapter recognizes a replaceable pattern
        v
Substrait candidate + planning context
        |
        | SDK validates; connector makes a plan-aware decision
        v
pushdown outcome + PlannedSubplan
        |
        | adapter rewrites the native tree and retains required operators
        v
native residual tree over a connector scan
~~~

The adapter owns native pattern matching, candidate granularity, native
residual operators, and native plan replacement. The connector owns
source-specific semantic checks and execution of an accepted remote subplan.
The SDK owns the common Substrait boundary, outcome rules, persisted plan state,
and execution-time contract checks.

The shared contract does not require CNF or DNF normalization. Each adapter
offers a candidate at the granularity its planner can remove or retain safely.
Substrait remains the only shared relation and expression tree; the SDK does not
translate it into a second `ColumnRef`, `Condition`, or operator AST.

## Planning lifecycle

One planning attempt follows this sequence:

1. The adapter recognizes a native operator or composite planner pattern.
2. It serializes the corresponding Substrait candidate and supplies the
   `PushdownRule`, `ResidualPolicy`, and `PlanningContext`.
3. The SDK validates the Substrait version, plan shape, extensions, schema, and
   resource bounds, producing a `ValidatedCandidate`.
4. The SDK dispatches the candidate to the connector method for that rule.
5. The connector returns `Exact`, `Retained`, or `Unsupported` after evaluating
   the concrete plan and context.
6. The SDK packages an accepted candidate as a `PlannedSubplan`; the adapter
   then performs the corresponding native plan rewrite.

Static capabilities may prune attempts that are certainly unsupported, but
they cannot authorize a rewrite. The plan-aware connector method is
authoritative.

## Planning state

`PlanningContext` captures source and session semantics that can affect
correctness: source product and version, execution domain, collation, time
zone, decimal semantics, null ordering, snapshot identity, and security
identity.

`ValidatedCandidate` is the transient, validated form offered to a connector
rule. It contains the Substrait plan, its Analytics output schema, its source
bindings, and whether the candidate requires a retained native operator.

`PlannedSubplan` is durable state that an adapter can store in a scan or table
handle. It contains the accepted Substrait plan, output schema, source bindings,
and a validation fingerprint. The fingerprint binds the serialized plan to the
contract version and planning context under which it was accepted. Restoring a
serialized plan revalidates it and rejects a mismatched fingerprint.

Persisted planning state must not contain credentials, open connections, or
process-local pointers.

## Outcomes and residual operators

Every connector rule returns one of three outcomes:

- `Exact` means the connector guarantees the candidate semantics, so the
  adapter may remove the corresponding native operator.
- `Retained` means the remote operation is useful only as an optimization, so
  the adapter must retain the corresponding native operator.
- `Unsupported` means the adapter leaves both the native and remote plans
  unchanged.

Each attempt declares whether the adapter can retain the native operator. The
SDK rejects `Retained` when the rule requires exact replacement. It also
prevents an exact filter result from removing a native operator when the
Substrait candidate uses `ReadRel.best_effort_filter`.

The connector does not return a second SDK expression tree for residual
evaluation. The Query Engine evaluates residual logic in its native plan.

## Operator rules

The connector planner exposes methods for native planner patterns rather than
claiming context-free support for operator names.

The initial implementation accepts read, projection, and filter attempts.
Projection uses `ReadRel.projection` for column pruning. Filter uses
`ReadRel.filter` for exact predicates and `ReadRel.best_effort_filter` when a
native filter must remain.

The API reserves separate rules for limit, aggregation, sort, TopN, and join.
They default to `Unsupported` until their validation, execution, and adapter
rewrite paths exist. TopN remains distinct from sort and limit because some
Query Engines negotiate the combined pattern atomically.

## Query Engine adapters

The common contract does not imply that every engine can offer every rule.
Adapters map the hooks and guarantees available in their native planners onto
the shared outcomes.

| Query Engine | Adapter integration model |
| --- | --- |
| Spark | Stores planned state in its scan builder or scan and maps source pushdown callbacks to connector rules. |
| Trino | Stores planned state in a connector table handle and returns no change when an attempt is unsupported. |
| DataFusion | Maps `TableProvider` projection and filter facilities to initial rules; broader operators need optimizer or planner extensions. |
| DuckDB | Maps table-function projection and filter facilities; operators without hooks need an optimizer extension. |

Engine-specific callback behavior, residual guarantees, supported planner
patterns, and reviewed versions are documented in
[Query-engine pushdown integration](query-engine-pushdown.md).

## Invariants

Changes to the planning API or an adapter must preserve these properties:

- the adapter alone rewrites the Query Engine's native plan;
- a connector decision applies to one validated plan and planning context, not
  to an operator name in isolation;
- only `Exact` permits removal of the corresponding native operator;
- `Retained` is valid only when the attempt permits a residual operator;
- unsupported attempts do not mutate native or remote planning state;
- Substrait remains the shared plan AST across the boundary;
- persisted plans are serializable, revalidatable, and free of live resources
  and credentials; and
- adding an operator requires validation, connector planning, execution, and
  adapter rewrite support before it can report acceptance.

The rationale for using Substrait instead of an SDK-owned expression and
operator tree is recorded in
[Use Substrait as the shared pushdown AST](design/use-substrait-as-pushdown-ast.md).
