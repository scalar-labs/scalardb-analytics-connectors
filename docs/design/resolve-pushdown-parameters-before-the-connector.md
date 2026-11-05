# Resolve pushdown parameters before the connector

## Design question

Should a connector receive dynamic parameters and their bindings in a
pushdown request, or should the Query Engine adapter resolve them to literals
before offering the request?

## Decision

The connector accepts only non-parameterized pushdown requests. A Query Engine
adapter resolves every native runtime parameter used in an offered region to a
typed Substrait literal. The connector rejects Substrait dynamic-parameter
expressions and non-empty plan-level `parameter_bindings`.

A SQL Connector renders those typed literals with its source-specific dialect
and produces one complete non-parameterized SQL statement. The connector
execution boundary does not carry a separate parameter collection.

## Rationale

Runtime parameters belong to the Query Engine's query representation and
execution lifecycle. Query Engines expose them differently: an adapter may see
an already substituted literal or a parameter expression with a bound value.
That difference does not need to cross the connector boundary. When the value
is available for pushdown planning, the adapter can express the same value as
a Substrait literal. When it is not available, the adapter cannot offer that
parameterized expression under this profile and must keep the work in the
Query Engine.

The connector plans a concrete remote operation for one request. Preserving a
dynamic parameter after its value is known would add another representation
and lifecycle across Substrait validation, connector planning, executable
query construction, and Data Source execution. It would also require the
connector to reconcile a parameter's declared Substrait type, bound value, SQL
placeholder, and driver binding type. The accepted literal already carries the
value and type required by planning and dialect rendering.

Parameterized source execution is therefore not part of the connector
contract. Avoiding it keeps parameter binding out of the common SDK, SQL
translation, and Data Source access boundary instead of requiring every SQL
Connector to implement behavior that the pushdown request does not need.

## Consequences

The adapter owns conversion from a native bound parameter to a Substrait
literal. Adapter integration tests cover that conversion for each supported
Query Engine.

The connector SDK validates literals and rejects both dynamic-parameter
expressions and plan-level bindings. `LogicalPlan` has no parameter-binding
state. SQL dialects own typed-literal spelling and escaping, and ADBC execution
receives SQL without a parameter batch.

Because literal values appear in generated SQL, executable payloads and
literal-bearing boundary plans are treated as sensitive and remain absent from
ordinary logs and connector-composed errors. Their public boundary types do not
implement `Debug`; the internal `LogicalPlan` debug representation redacts
literal values.
