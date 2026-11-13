# Compare table metadata in the library

## Design question

Receivers such as the Physical Catalog and the Semantic Catalog detect source
changes by comparing metadata they stored with newly retrieved metadata. Who
defines and performs that comparison, what does it depend on, and what does it
report?

## Decision

The library performs the comparison, split by what is compared, and reports
only facts. The rules are in
[Comparing table metadata](../connector-contract.md#comparing-table-metadata).

- **Facts only.** A comparison reports values observed on both sides that
  differ. An item either side did not observe, such as a missing or not
  offered item, an unread Data Source type, or whether columns may be hidden,
  is not compared. Nothing is inferred: a column on one side only is added or
  removed whatever the reason, and what a difference means (a drop, a rename,
  a change of privileges) is for the receiver to decide.
- **No difference, no result.** Both comparisons return no result when
  nothing differs, and otherwise every part, with empty parts for those
  without differences. A receiver's first question, whether anything differs,
  is one check.
- **Table metadata** has one shape for every connector, so the connector SDK
  compares it without any connector (`TableMetadata::compare`). Only tables
  that were read on both sides are compared; that a table does not exist or
  could not be read is already known to the caller that paired the records.
- **Source metadata** has its connector's shape, so its comparison belongs to
  the connector component that defines that shape:
  `Catalog::compare_source_metadata`, provided by the SDK for every catalog. It
  compares the two values as JSON documents, which covers what table metadata
  does not hold, and compares the table metadata derived from both. Source
  metadata carries its connector's name; the library restores both documents
  through the built-in connector of that name, and documents of different or
  unknown connectors, or an unrestorable document, are an invalid request.
- **Matching.** Columns are matched by source identifier only when both tables
  have the same source identifier and both columns have one, and by name
  otherwise; constraints by kind and name. The table's kind and source
  identifier are compared; its name is how the caller paired the records.
- **Document differences.** JSON Pointer paths, object members by name, and
  array elements matched by equality along their longest common subsequence.
  Between two matched elements, unmatched elements are paired in order, so an
  element that changed in place is reported once with both versions rather
  than as a removal and an addition; the rest are added or removed.
- **The connector API** compares one pair per call, and neither comparison
  takes a handle. Both take the typed C structures that a table metadata
  request returns, so a retrieved record's table metadata or source metadata
  is passed as it is; a caller that stored metadata in its own form builds the
  same structures from it.
- Each receiver stores what it uses: the Physical Catalog stores table
  metadata, and the Semantic Catalog stores source metadata. Retrieval info is
  never compared.

## Rationale

Matching items and reading incomplete metadata are decisions; if each
receiver made them, one source change would become different changes in the
Physical Catalog and the Semantic Catalog. Placing the comparison in the
library gives these decisions one owner beside the contract that defines the
states they interpret.

A connector observes only that a catalog query returned rows or failed. It
cannot observe privileges, or whether a column is missing from a result
because it was dropped or because it is no longer visible. Rules that labeled
such cases "unknown", or that reported states beside differences, mixed
observations with inferences, and a receiver could not tell which parts of a
result to trust. Comparing only what both sides observed keeps every reported
difference true. A dropped and a no-longer-visible column are both columns the
receiver can no longer use, so reporting either as removed lets the receiver
act correctly.

The receivers' decisions shaped the result. The Physical Catalog re-registers
a table when anything differs, and the Semantic Catalog reinterprets it, so
whether anything differs must be readable at once. A separate "same" flag
next to item differences would be a second answer to that question, and for
source metadata it would disagree with item differences whenever only a
source-only attribute changed. Returning no result when nothing differs, and
document differences for source metadata, makes the one check complete.

Column identifiers such as PostgreSQL `attnum` are unique only within one
table. Matching by them across a re-created table would report a different
column as a rename, so they are used only when the table identifiers agree.

Whether a comparison depends on a connector follows from who knows the shape
of what is compared. Table metadata is the SDK's model, so its rules live once
in the SDK. Source metadata is the type a connector's `Catalog` declares, so
its comparison is a function of that component, while its body is shared by
every catalog. The comparison takes restored values rather than text: a caller
that already holds a value, such as a fresh retrieval, passes it directly, and
the text representation stays at the boundary that receives it. Comparing
re-serialized values also keeps a stored document's formatting, such as key
order after a database reformats JSON, out of the result.

The comparison's inputs are the structures a retrieval returns. An earlier
form took table metadata as Arrow rows, so a caller had to slice a retrieved
batch to one row, rebuild its schema, and wrap it in a generic Arrow structure
before it could compare what it had just retrieved, and nothing at the
boundary said that the input was table metadata. Typed structures remove that
conversion. Source metadata names the connector that produced it, so the
library can restore it without a data source, and a caller can compare stored
documents without connecting.

One pair per call keeps a failure, such as one unrestorable document, to its
own pair, and matches what callers hold: records stored per table.

## Alternatives not taken

- **Each receiver compares on its own.** This keeps the library smaller but
  lets receivers disagree about the same change, which is the problem being
  solved.
- **Reporting "unknown" items and table states with the differences.** It
  infers what was not observed and makes a result partly untrustworthy.
- **Comparing only when both sides are complete.** It reports nothing for a
  read whose privileges never cover some items, although the observed items
  can still be compared.
- **A "same" flag beside the differences.** It is a second, possibly
  disagreeing, answer to whether anything differs.
- **Comparing source metadata by equality only.** A source-only change would
  then show as "not the same" with empty differences, so the receiver could
  not see where it changed.
- **Connector-defined differences of source metadata.** Each connector would
  need a difference format; a generic document difference needs none.
- **The comparison on `Connector` or on `SourceTableMetadata`.** `Connector`
  holds flows across components, which this is not, and the type that holds
  source metadata is declared by the `Catalog` component.
- **Arrays of pairs per call.** One bad pair would fail every other pair, and
  callers would have to assemble arrays from per-table records.
- **Table metadata as Arrow rows and a generic Arrow input structure.** It
  made callers convert what they retrieved and gave the boundary no type for
  table metadata.
- **Taking a data source handle for the source metadata comparison.** Source
  metadata names its connector, and the comparison needs no connection; a
  handle would only make callers open a data source to compare stored
  documents.

## Consequences

- `SourceTableMetadata` requires serialization and deserialization, so every
  connector's source metadata type can be restored from its document and
  compared as a document.
- The connector API has two comparison functions with one result shape.
- A library or connector update that changes the table metadata structures
  or the source metadata shape makes previously stored metadata uncomparable
  until cross-version rules are added.
