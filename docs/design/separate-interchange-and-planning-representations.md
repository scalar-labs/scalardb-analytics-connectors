# Separate interchange and planning representations

## Design question

Should pushdown planning operate directly on the Substrait protobuf model used
at the connector boundary, or should it use a separate internal
representation?

## Decision

Use Substrait as the language-neutral interchange representation at the
connector boundary. Validation constructs `LogicalPlan`, the connector SDK's
internal representation for pushdown planning, from the accepted Substrait
shape. Rewrite rules operate on `LogicalPlan`; completed remote and optional
local plans are converted back to Substrait before they cross the connector
boundary.

`LogicalPlan` is a strictly typed representation of the Substrait semantics
accepted by the SDK. It is a semantic subset of Substrait, not a Rust subtype
of the generated Substrait protobuf types. Its types exclude states that the
accepted plan shape considers structurally invalid while preserving the
semantics required by that shape.

Within the SDK-supported profile, conversion between a validated Substrait plan
and `LogicalPlan` is semantically lossless. Converting a validated plan to
`LogicalPlan` and back preserves its query meaning and the metadata required to
interpret it. This does not promise identical protobuf bytes, preservation of
unknown protobuf fields, or preservation of distinctions that have no semantic
effect in the supported profile.

## Rationale

Substrait already provides the language-neutral relational and expression
vocabulary required between Query Engine adapters and connectors. Replacing it
at that boundary would create another interchange format and require every
implementation language to reproduce Substrait's semantics.

The generated protobuf model is less suitable for planning code. Required
logical children and mutually exclusive relation variants remain represented
through optional fields, so states rejected by validation are still
constructible. Operating rewrite rules directly on that model spreads
validation assumptions, unwraps, and protobuf representation details through
the planning implementation.

A strictly typed internal representation gives rewrite rules only valid states
from the supported semantic profile. It also isolates rule semantics from
protobuf layout changes. Keeping this representation semantically aligned with
Substrait avoids defining an independent query language: it models only the
profile accepted at the boundary and must round-trip that profile without
changing query meaning.

The internal `Read` intentionally omits Substrait's scan-local `filter` and
`projection` fields. In this SDK, pushdown planning means decomposing a query
into remote and local plans. Filters and projections therefore remain explicit
operators whose placement shows which side executes them. Encoding a shipped
operator inside `Read` would mix query decomposition with a second, scan-local
kind of pushdown and would give the internal model two representations of the
same operation.

An internal `Read` retains both the accepted Substrait `NamedStruct` and its
validated `AnalyticsSchema`. The former preserves the boundary representation
when a logical plan is converted back to Substrait; the latter is the schema
used by planning and contract checks, so those stages do not parse a validated
input schema again.

Pushdown planning is a sequence of tree transformations. `LogicalPlan` is
therefore immutable through the Connector-facing API: a rewrite describes the
next relation tree and constructs another complete plan instead of modifying
the current plan in place. Maintaining plan-level invariants is not a
responsibility repeated in every transform. `LogicalPlan` construction derives
and stores the output schema from the replacement tree and root names. Avoiding
a stale schema is one consequence of that responsibility boundary.

The rejected alternatives are:

- Using the generated Substrait types throughout planning. This avoids a
  conversion but makes every planning operation responsible for invariants
  already established at validation.
- Defining an SDK-owned interchange AST. This would duplicate Substrait across
  language boundaries and make adapters depend on a connector-specific query
  language.

## Consequences

Validation is also the conversion boundary: each accepted Substrait relation
is validated and used to construct its `LogicalPlan` relation in the same
pass. Successful validation produces a `LogicalPlan`; there is no second
validation-result carrier and no second traversal that assumes the protobuf
tree is valid. Connector rewrite rules and
`QueryPlan` use that representation rather than generated protobuf relation
nodes.

The SDK owns conversion in both directions and tests semantic round trips for
every accepted shape. Extending the accepted Substrait profile requires adding
the corresponding strictly typed representation and both conversions before a
rewrite rule can use the new semantics.

Validation applies to the externally offered Substrait plan. Remote and local
plans are produced internally by Connector rewrite rules, so
`PlannedPushdown` construction asserts their decomposition contract instead of
reporting their defects as invalid external input. The assertion contract is
defined on the construction method: the remote plan ends in a read, the
optional local plan ends in a schema-matched `RemoteResult`, and the composed
output schema equals the offered output schema.

Substrait remains visible in `PushdownRequest`, `PlannedPushdown`, and the
language-neutral connector contract. `LogicalPlan` is an implementation detail
of connector planning and is not a second wire format.
