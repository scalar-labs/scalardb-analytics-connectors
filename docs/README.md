# Developer documentation

This directory contains established context for developers who change or review
ScalarDB Analytics Connector. It explains the current implementation and
accepted architectural direction; it is not a product-user guide, work log, or
implementation inventory.

## Reading path

Start with [architecture.md](architecture.md) for the repository-wide
vocabulary, boundaries, ownership, and representative data flow. Follow its
links to the relevant top-level document before changing an architectural unit.

Use the remaining top-level documents by responsibility:

- [connector-contract.md](connector-contract.md) defines what callers and
  connector implementations may rely on across the connector boundary.
- [analytics-types.md](analytics-types.md) defines the shared type-system mental
  model; [type-mappings.md](type-mappings.md) applies it to Connectors.
- [pushdown-planning.md](pushdown-planning.md)
  defines the pushdown glossary and lifecycle, query decomposition, remote
  query generation and shipping, plan ownership, and the planning and execution
  interfaces.
- [query-engine-pushdown.md](query-engine-pushdown.md) maps the planner
  contracts of Spark, Trino, DataFusion, and DuckDB onto that shared model.
- [connector-internals.md](connector-internals.md) explains SDK, source-access,
  lifecycle, and assembly ownership.
- [connector-implementation.md](connector-implementation.md) gives the
  conventions for implementing and testing one Connector.

Read the shared pushdown model before changing the Substrait request contract,
connector planning boundary, or a Query Engine adapter. Then use the
engine-specific comparison to check the native hook, request granularity,
residual guarantee, and rewrite behavior.

The focused decisions under [design](design/) explain why the type model is
flat, why one connector interprets each source, why the boundary is a typed C
ABI rather than ADBC, why pushdown planning separates its Substrait interchange
representation from its internal representation, and why a blocking ADBC poll
temporarily owns its query resources.

Use the documentation layers for distinct purposes:

| Location | Owns |
| --- | --- |
| Top-level docs files | A coherent mental model for one architectural unit, including its purpose, relationships, flows, and invariants |
| docs/design | The current answer and rationale for one explicit design question |
| Issue tracker | Proposals, implementation gaps, investigation, execution status, and completion records |
| AGENTS.md | Operational instructions agents must follow |

## Placing and maintaining context

Put a claim in a top-level document when developers need it to predict where
behavior belongs, trace an important flow, or preserve a boundary. Put it in
design only when it answers one question whose rationale will help a future
developer protect or reconsider the answer.

Give each settled claim one canonical home. Other documents may summarize it
only as deeply as their readers need and should link to the canonical
explanation. Keep proposals, migration narration, and work status in the issue
tracker.

Treat documentation as part of an architectural change. When a domain model,
component boundary, dependency direction, representative flow, or binding
invariant changes, reconcile the affected documentation as one coherent current
state. Verify claims against code, contracts, and representative tests before
completion.
