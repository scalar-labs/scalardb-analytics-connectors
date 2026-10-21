# Developer documentation

This directory contains established context for developers who change or review
ScalarDB Analytics Connector. It explains the current implementation and
accepted architectural direction; it is not a product-user guide, work log, or
implementation inventory.

## Reading path

Start with [architecture.md](architecture.md) for the repository-wide
vocabulary, boundaries, ownership, and representative data flow. Follow its
links to the relevant top-level document before changing an architectural unit.

Read [query-engine-pushdown.md](query-engine-pushdown.md) before changing the
Substrait request contract, connector planning boundary, or a Query Engine
adapter. It compares the pushdown contracts of Spark, Trino, DataFusion, and
DuckDB and derives the constraints on their shared model.

Read
[plan-pushdown-with-query-engine-planners.md](plan-pushdown-with-query-engine-planners.md)
for the accepted ownership model, planning API, and pushdown outcomes.

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
