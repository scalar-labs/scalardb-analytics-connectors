# Use Substrait as the shared pushdown AST

## Design question

Should the connector SDK define its own request, expression, and operator AST,
or should Query Engine adapters and connectors negotiate pushdown using
Substrait plans?

## Decision

Use Substrait relation and expression messages as the shared plan AST. Do not
translate them into SDK-owned `ColumnRef`, `Condition`, or operator trees.

An adapter constructs a Substrait candidate for a native planner pattern. The
SDK validates the common contract, and the connector evaluates the concrete
candidate together with its source and session context. The adapter remains
responsible for the native plan rewrite and any residual operator.

## Rationale

An SDK-owned AST would duplicate information already represented by Substrait.
Every translation would need to preserve expression semantics, null behavior,
types, extensions, and eventually relational operators. The additional type
system would constrain future pushdown shapes without removing the need for
connector-specific validation.

Support is not a fixed property of an expression or operator. It can vary with
the complete candidate, data type, collation, Data Source version, session settings,
security context, and execution domain. A static capability description can
avoid obviously futile attempts, but only plan-aware validation can authorize
a rewrite.

Keeping Substrait at the boundary also lets adapters reuse available Substrait
conversion infrastructure. An adapter only needs to construct the candidate
pattern its native planner can safely remove or retain; it does not need to
convert the Query Engine's entire plan.

## Consequences

Connectors depend on the pinned Substrait contract and must reject both invalid
plans and valid plans whose operations or semantics they cannot execute.
Adapters must explicitly translate their native planner patterns into the
accepted Substrait subset.

The shared contract does not impose CNF, DNF, or another expression normal
form. Candidate granularity follows the native planner rule, and residual
evaluation remains in the Query Engine rather than in a second SDK expression
evaluator.

This representation can grow from projection and filter to limit,
aggregation, sort, TopN, and join without defining a parallel connector AST.
The current planning model and its invariants are documented in
[Plan pushdown with Query Engine planners](../plan-pushdown-with-query-engine-planners.md).
