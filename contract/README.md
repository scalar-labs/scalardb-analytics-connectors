# Connector contract representation

This directory is the language-neutral representation of the connector
contract. Product architecture documents define the contract's meaning; files
here define the C ABI and machine-readable projections consumed by connector
implementations and callers.

The initial skeleton reserves these stable locations:

- `connector_api.h`: C API declarations and contract version constants.
- `requests/`: Arrow schema descriptions for request and response streams.
- `declarations/`: JSON Schemas for capability and static-information documents.
- `substrait/`: the accepted version, plan shape, type correspondence, and the
  logical-type extension.
- `logical-types/`: the registered logical-type inventory.
- `type-mappings/`: source-specific mapping tables.

`manifest.json` is the inventory used by `scripts/validate_contracts.py`. Empty
logical-type and type-mapping lists are explicit implementation placeholders;
their authoritative contents remain in the product design until later contract
implementation work projects them here. The accepted Substrait version is not
yet pinned.
