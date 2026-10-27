# Model Analytics types as one federation type system

## Design question

Should the Analytics schema divide types into physical and logical layers, or
represent every federation type in one type system?

## Decision

Represent every Analytics type in one source-independent federation type
system. Int, Text, Uuid, Enum, and structured types are peer variants of
AnalyticsType.

Arrow storage types and extension metadata belong to the Analytics-to-Arrow
mapping. Source-native types belong to connector mappings, and Query Engine
types belong to adapter mappings.

## Rationale

The Analytics schema unifies concrete type systems at federation boundaries.
Calling part of that schema physical imports Arrow's storage-versus-extension
distinction into the domain model and suggests a subtype relationship that the
federation does not have.

A UUID may originate as a native PostgreSQL type, travel as Arrow Utf8 with
extension metadata, and become a native DuckDB UUID or a Spark string fallback.
Those representations do not change its Analytics meaning. Keeping the
Analytics type independent makes each boundary mapping explicit and prevents
Arrow compatibility mechanics from defining the public schema model.

## Consequences

Consumers match one AnalyticsType enum. Types whose Arrow representation needs
extension metadata have stable names and parameter encodings in the Analytics
type model and conversion implementation. Code that needs those details asks
the Arrow conversion boundary rather than traversing a physical/logical split.
