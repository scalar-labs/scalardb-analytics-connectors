# PostgreSQL connector

The PostgreSQL connector reads PostgreSQL through the Apache Arrow ADBC
PostgreSQL driver (ADBC access). It reads `pg_catalog` itself and maps types by
[the PostgreSQL type mapping](../../../docs/type-mappings.md#postgresql). The
general conventions it follows are in
[Initial PostgreSQL and MySQL conventions](../../../docs/connector-implementation.md#initial-postgresql-and-mysql-conventions).

## Data Source access

| Item | Value |
| --- | --- |
| ADBC driver | `adbc_driver_postgresql`, loaded through its driver manifest `adbc_driver_postgresql.toml` in the library's ADBC driver directory |
| Supported driver versions | `>=1.13.0, <1.14.0` |
| Connection options | `host`, `port`, `database`, `user`, `password` (secret), `sslmode`, and `search_path` |
| Fixed session settings | `TimeZone=UTC`, `client_encoding=UTF8`, and `standard_conforming_strings=on` |
| Fixed driver options | `adbc.postgresql.use_copy=true` and `adbc.postgresql.type_resolver_mode=server` |

The driver reports its version as `unknown`, so the version comes from the
driver manifest.

`search_path` decides how Data Source types are spelled: `format_type`
qualifies a user-defined type, such as an enum or a domain, with its schema
unless the type is on the path. Without the option, the server's setting for
the role and database applies.

## Coverage

Version 0.1.0 covers every mapping rule in the PostgreSQL type mapping. The
following values cannot be carried exactly and are `data` errors when a read
returns them:

- `numeric` `NaN` and infinities;
- `time` `24:00:00`.

## Pushdown

| Offered work | Runs remotely when |
| --- | --- |
| Filter | Every comparison in it is between operands of one type among Boolean, integers, decimals, dates, times, timestamps, and UUIDs, or is text equality on a `text` or `character varying` column with a deterministic collation. The whole filter moves, or none of it does. |
| Projection | It selects plain columns and no filter remains local below it |

Floating-point comparisons, text ordering, `character(n)` comparisons, enum
ordering, and interval comparisons stay in the local plan.

## Known limitations

These come from the driver and are not yet handled by the connector:

- `date`, `timestamp`, and `timestamp with time zone` values of `infinity` and
  `-infinity` are returned as wrong dates or timestamps instead of a `data`
  error.
- An interval whose time part does not fit in nanoseconds fails the read as a
  `data-source` error rather than a `data` error.
- A failed authentication is reported without a SQLSTATE, so it is classified
  as `connection` rather than `authentication`.
- A `timestamp` value beyond the range of Arrow microsecond timestamps, such
  as `294276-12-31 23:59:59.999999`, is returned as a wrong timestamp instead
  of a `data` error.

These come from the connector and its ADBC access:

- Releasing a read stream partway drops the driver's reader without
  cancelling its statement, so the query keeps running at the server.
- A connection the server terminates, which PostgreSQL reports with SQLSTATE
  `57P01`, is classified as `data-source` rather than `connection`, so the
  session stays usable until its next request fails.

The [conformance suite](#tests) checks each of these as a known deviation and
fails once the connector meets the contract.

## Compatibility

Only combinations that passed this crate's tests are listed.

| PostgreSQL | ADBC driver | Platform |
| --- | --- | --- |
| 18.6 | 1.13.0 | macOS arm64 |

## Tests

The integration tests start a PostgreSQL container with Docker and load the
driver from `ADBC_DRIVER_DIRECTORY`, which the Nix development shell sets.
`tests/conformance` runs the SDK's connector conformance suite with this
connector's fixture:

```sh
nix develop --command cargo test -p connector-postgresql
```

`POSTGRESQL_TEST_IMAGE` selects another server image, such as `postgres:14`.
The container is kept between runs and can be removed with `docker rm -f`.
