# Compare table metadata in the library

## Design question

Receivers such as the Physical Catalog and the Semantic Catalog detect source
changes by comparing metadata they stored with newly retrieved metadata. Who
defines and performs that comparison, what does it depend on, and what does it
report?

## Decision

The library performs the comparison, split by what is compared. The rules are
in [Comparing table metadata](../connector-contract.md#comparing-table-metadata).

- **Table metadata** has one shape for every connector, so the connector SDK
  compares it without any connector (`TableMetadata::compare`, and
  `TableRecord<TableMetadata>::compare` with the table states). It reports
  item by item what was added, removed, changed, or cannot be known.
- **Source metadata** has its connector's shape, so its comparison depends on
  that connector. The connector restores a stored document to its source
  metadata type; two values are the same exactly when they are equal. What
  changed is found by comparing the table metadata derived from both. A side
  that does not exist or could not be read follows the same state rules as
  table metadata, so no receiver decides a drop on its own.
- Each receiver stores what it uses: the Physical Catalog stores table
  metadata, and the Semantic Catalog stores source metadata. No stored record
  holds both, because table metadata is derived from source metadata.
- Source metadata and table metadata hold only what the source states. The
  retrieval time travels beside them in `TableRecord::Read`, and no
  comparison uses retrieval info.
- Columns are matched by source identifier when both sides have one, and by
  name otherwise, so a rename is distinguished from a drop and re-creation
  whenever the source provides identifiers.
- Missing, unread, possibly hidden, and one-sided "not offered" information,
  and unmatched columns that may be one renamed column, yield "unknown", never
  a removal or change.

## Rationale

Determinism makes comparison meaningful, but it does not say what a difference
means. Matching items, ignoring retrieval-only values, and reading incomplete
metadata are decisions; if each receiver made them, one source change would
become different changes in the Physical Catalog and the Semantic Catalog. The
rule that incomplete metadata never implies a drop would also have to be
reimplemented, and could be broken, by every receiver. Placing the comparison
in the library gives these decisions one owner beside the contract that defines
the states they interpret.

Whether a comparison depends on a connector follows from who knows the shape
of what is compared. Table metadata is the SDK's model, so its rules live once
in the SDK. Source metadata is the connector's own type, so only the connector
can read a stored document back. Equality of that type is exact under
determinism. Item-level differences come from the derived table metadata, so
no connector needs its own difference format.

Storing both layers together would duplicate a derivation and allow the two to
disagree. Keeping the retrieval time inside the compared values would make
equal source state compare unequal and force a restored value to invent a
time; retrieval time is retrieval info, not content.

The comparison reports changed items rather than one boolean so that a
receiver can limit reinterpretation to what actually changed.

## Alternatives not taken

- **Each receiver compares on its own.** This keeps the library smaller but
  lets receivers disagree about the same change, which is the problem being
  solved.
- **A content digest or source schema version.** Removed earlier because it
  only says that something changed and needs its own versioning; it cannot tell
  which items changed.
- **One stored record holding both layers and a producer.** It duplicates the
  derivation of table metadata. The producer is retrieval info that callers
  can compare themselves.
- **Connector-defined differences of source metadata.** Each connector would
  need a difference format whose use is not yet known; the derived table
  metadata already reports item-level changes.

## Consequences

- `SourceTableMetadata` requires deserialization and equality, so every
  connector's source metadata type can be restored from its document and
  compared.
- The connector API gains two comparison functions: one for table metadata,
  independent of Connectors, and one for source metadata, dispatched to the
  named Connector. Neither needs a data source or session.
- A library or connector update that changes the table metadata output schema
  or the source metadata shape makes previously stored metadata uncomparable
  until cross-version rules are added.
