# Make one connector interpret each data source

## Design question

Which component should decide a data source's column types and produce its
values, and how should Query Engines obtain them?

## Decision

One connector owned by ScalarDB Analytics is the only interpreter of a data
source. It retrieves the source's metadata, decides each column's Analytics
type, translates read requests, and produces values typed by those Analytics
types. Query Engines never touch the data source; each one maps connector
output into its own types through an adapter.

The Physical Catalog persists the Analytics types the connector returned at
registration, together with the source types. A column whose source type
cannot be mapped is still registered, as an unsupported column. At read start
the connector compares the registered types with the types the source reports
now and fails with a stale-registration error instead of casting the
difference away.

## Problems addressed

Before this decision, each Query Engine reached a data source through its own
path. The catalog server read JDBC `DatabaseMetaData` and mapped it through a
per-source strategy; the Spark integration read rows through Spark's JDBC path,
where the driver converted values to the planned type; the DuckDB experiment
read schemas and rows through DuckDB scanner extensions. This arrangement had
four problems:

1. **The same column had different types per Query Engine.** A MySQL
   `TIMESTAMP` was reported with a time zone by the JDBC driver and read
   without one by the DuckDB scanner.
2. **Registered types were not guaranteed to match readable values.** Spark's
   driver getters converted implicitly to the planned type and failed at run
   time when they could not. DuckDB planned with the registered type but the
   scanner produced values by its own rules.
3. **Type correspondence depended on driver reports.** The MySQL strategy
   followed the JDBC driver's default report of `TINYINT(1)` as a boolean, so
   changing a driver setting changed the registered schema.
4. **Columns without a mapping disappeared.** The schema resolver skipped
   columns of unknown types, leaving only a gap in the ordinals.

Problems 1 and 2 arise because the component that decides the schema differs
from the component that produces the values. Problem 3 arises because schema
retrieval was not under ScalarDB Analytics' control. Problem 4 arises because
being mappable was a condition for registering a column.

## Principles

The decision follows from four properties the design must have:

- **A: One interpreter per data source.** The component that retrieves a data
  source's metadata and decides its types also produces its values. As long as
  the two are separate, a registered type is only a wish, and agreement with
  readable values cannot be guaranteed.
- **B: The same column has the same type and value under any Query Engine.**
  This applies to Analytics types and values, before a Query Engine maps them.
  A loss a Query Engine declares when mapping to its own types, such as a
  string for a UUID, is part of that engine's adapter and outside this
  principle.
- **C: Registered types are sufficient for planning without contacting the
  source.** A coordinator that binds tables of remote nodes, and a data mesh
  node that reads another node's catalog, cannot plan otherwise. An embedded
  deployment, which could plan with types it reads itself, uses the same types
  so that moving it to a server deployment does not change types or values.
- **D: The source's declaration is neither dropped nor silently changed.** A
  declared column remains registered even when its type cannot be mapped.
  Type correspondence comes from the source's own catalog, not from a driver's
  reinterpretation, and the declaration itself is kept as the source type.

The connector satisfies A because it both retrieves metadata and produces
values. Every Query Engine reads through the same connector, which satisfies B.
Persisting Analytics types in the Physical Catalog satisfies C. Returning
unmappable columns with their source types, read from the source's own
catalog, satisfies D.

The schema check runs in the connector because it is closest to the source's
declaration and runs before any value is produced. Casting the difference
instead would make the casting component a second interpreter, breaking A,
and would leave the Physical Catalog advertising types that no longer hold.

## Alternatives not taken

| Alternative | Why it is not taken |
| --- | --- |
| Let each Query Engine read through its own path and cast to the registered types in the adapter | Two components interpret the same source type and the cast hides the difference, violating A. Agreement depends on each cast being correct, and every new Query Engine needs its own casts, violating B. |
| Let the Query Engine decide the schema and keep only names in the Physical Catalog | Planning without contacting the source becomes impossible, violating C, and types vary by Query Engine, violating B. |
| Store only source types and let each Query Engine map them dynamically | Server deployments and data mesh still need a common type (C), so one would be defined somewhere anyway. Defined outside the Physical Catalog, it would be invisible to catalog browsing. |

## Consequences

Connectors cannot reuse the broad source coverage of DuckDB scanner extensions
for reads. Coverage instead grows through connectors, primarily through ADBC
drivers and the shared SQL-family processing, and through the generic ODBC and
Query Engine-backed tiers described in the
[connector contract](../connector-contract.md#support-tiers).
