# Connector contract representation

This directory is the language-neutral representation of the connector
contract. The [connector contract](../docs/connector-contract.md) defines the
contract's meaning; files here define the C ABI and machine-readable
projections consumed by connector implementations and callers.

The initial skeleton reserves these stable locations:

- `connector_api.h`: C API declarations and contract version constants.
- `requests/`: Arrow schema descriptions for request and response streams.
- `declarations/`: JSON Schemas for capability and static-information documents.
- `substrait/`: the accepted version, plan shape, type correspondence, and the
  logical-type extension.
- `analytics-types/`: language-neutral Arrow encodings for Analytics types.
- `type-mappings/`: source-specific mapping tables.

`manifest.json` is the inventory used by `scripts/validate_contracts.py`. The
Analytics type artifacts define stable extension names, parameters, value
representations, and Arrow storage encodings. The
[developer documentation](../docs/analytics-types.md) owns the type-system
mental model. Empty type-mapping lists remain explicit implementation
placeholders. The accepted Substrait version is pinned to 0.69.0. The
machine-readable contract also pins the Rust binding and validator versions,
the initial planner rules and outcomes, the accepted read-plan shape, persisted
planned-subplan state, planning context, and logical-type encoding. Substrait
remains the shared relation and expression AST; Query Engine adapters own native
residual operators.
