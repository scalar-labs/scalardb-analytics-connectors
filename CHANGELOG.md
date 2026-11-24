# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- DynamoDB connector, through the AWS SDK for Rust.

### Changed

- The minimum supported Rust version is 1.94.1, which the AWS SDK for Rust
  requires.

## [0.1.0] - 2026-11-24

### Added

- Connector API library with a C API that lists namespaces and tables, reads
  table metadata, plans filter and projection pushdown, and reads the results
  as Arrow streams.
- PostgreSQL connector, through the Apache Arrow ADBC PostgreSQL driver.
- MySQL connector, through the ADBC Driver Foundry MySQL driver.
- Salesforce connector, through the Salesforce REST API with the
  `refresh_token` and `jwt_bearer` OAuth flows.
- SQL Server connector, through ODBC and the Microsoft ODBC Driver 18 for SQL
  Server.
- DuckDB v1.5.6 extension that attaches a Data Source as a read-only catalog,
  takes connection options from an ATTACH or a DuckDB secret, and pushes
  filters and projections down to the Data Source.
- Self-contained packages for `osx_arm64`, `linux_amd64`, and `linux_arm64`
  that contain the library, the DuckDB extension, the ADBC drivers, the C
  header, and the license texts. The Linux packages also contain the SQL
  Server connector with the ODBC driver manager and the libraries its driver
  needs; `bin/fetch-dependencies` fetches the Microsoft ODBC Driver 18 for SQL
  Server after the user accepts its license.

[Unreleased]: https://github.com/scalar-labs/scalardb-analytics-connectors/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/scalar-labs/scalardb-analytics-connectors/releases/tag/v0.1.0
