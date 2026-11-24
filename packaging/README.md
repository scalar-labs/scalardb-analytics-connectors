# Packaging

This directory builds and checks the
[distribution package](../README.md#distribution-package): one tgz per
platform that works wherever it is unpacked.

| File | Role |
| --- | --- |
| `dependencies.json` | The dependencies the connectors ship and the DuckDB extension is built with |
| `package.sh` | Builds the package of the running platform from that declaration |
| `fetch-dependencies` | Goes into the package's `bin/`: fetches the dependencies the package may not carry, as their manifests in `downloads/` describe ([Delivery](#delivery)) |
| `linux/package.sh` | Runs `package.sh` for Linux in the pinned manylinux_2_28 image with Docker |
| `linux/build.sh` | Prepares the manylinux container and runs `package.sh` in it |
| `linux/check.sh` | Runs `test/run-check.sh` on a Linux package from a host that is not Linux, in the same image with Docker, beside PostgreSQL and SQL Server containers for the e2e |
| `linux/engine.sh` | Pins the manylinux_2_28 image for the Docker engine's architecture, for `linux/package.sh` and `linux/check.sh` |
| `test/run-check.sh` | Checks a package on its platform, as CI does: downloads the pinned DuckDB CLI and JDBC driver and runs `test/check-package.sh` |
| `test/check-package.sh` | Checks an unpacked package: binary references, the extension loaded through DuckDB's JDBC driver, the DuckDB e2e, with `bin/fetch-dependencies`, showing the licenses and fetching each dependency, and each connector's package test |
| `test/LoadExtension.java` | Loads the packaged extension through DuckDB's JDBC driver, for `test/check-package.sh` |
| `about.toml`, `about.hbs` | cargo-about configuration and template for the Rust crates' license texts |

## What ships

The library is built with every connector feature of `connector-api`, the
features that enable a connector crate, that the running platform is declared
for, and its Rust license texts cover them.

`dependencies.json` declares what those connectors ship beyond the library,
and what the DuckDB extension is built with, in three parts:

- `connectors`: each key is a connector feature, which lists the entries it
  ships under `dependencies`, and, under `platforms`, the platforms it is
  built for when it is not built for all of them, as `sqlserver-odbc` is
  built for Linux only. A connector that ships nothing beyond the library
  and is built for every platform has no key.
- `extension`: the entries the DuckDB extension is built with, under
  `dependencies`, such as the static protobuf and abseil it links.
- `dependencies`: each entry has a `kind` and the fields that kind needs.
  An entry listed several times ships once. An entry no connector or the
  extension lists is not used yet.

A connector's README declares all its dependencies and whether each ships or
the user installs it
([Declaring dependencies](../docs/connector-implementation.md#declaring-dependencies)).
`dependencies.json` lists only the ones the package provides, as far as their
licenses permit
([Shipping and installing dependencies](../docs/connector-implementation.md#shipping-and-installing-dependencies)).
A dependency the user installs is not listed here.

### Delivery

An entry's `delivery` states when its files reach the package; both pin the
version by URL and hash:

- `bundled` (the default): `package.sh` fetches it and places it in the tgz.
- `downloaded`: for a dependency the package may not carry, such as an ODBC
  driver whose license does not permit redistribution. `package.sh` writes a
  manifest for each, `downloads/<name>.toml`, and puts `fetch-dependencies`
  in `bin/`; the user runs it after unpacking:

  ```sh
  bin/fetch-dependencies list                               # each dependency, what uses it, its license URL, whether it is fetched
  bin/fetch-dependencies license <name>                     # its license URL and the text fetched from there
  bin/fetch-dependencies fetch <name>... --accept-license   # fetch the named ones, accepting their licenses
  ```

  `fetch` fetches nothing without `--accept-license`. A dependency may offer
  several downloads of the same files, such as a vendor's deb and rpm, so that
  most hosts need no preparation: `fetch` uses the first the host can unpack,
  checks it against its hash, and places the files the manifest lists under
  the directory it names, keeping the files the package carries there.

  | Format | Unpacked with |
  | --- | --- |
  | `deb` | `dpkg-deb`, as on Debian and Ubuntu |
  | `rpm` | `rpm2archive`, `tar`, and `gzip`, as on Red Hat Enterprise Linux and Amazon Linux; or `rpm2cpio` and `cpio`, as on SUSE |

  A host with none of them is told what to install. `fetch-dependencies` also
  requires `curl`, `awk`, and `sha256sum` or `shasum`; it reads the manifests
  itself, so no JSON or TOML tool is needed.

  Every `downloaded` entry has the same fields, whatever its kind: `name`,
  `version`, `license` (the URL of the vendor's license),
  `downloads.<platform>`, a list of downloads in the order they are preferred,
  each with `url`, `hash` (SRI sha256), and `format`, and `files`, which maps
  each path in the unpacked download to its path below the kind's directory
  and is the same for every download. The manifest holds the platform's
  values, the hashes as hex, the connectors that use it, and that directory:

  ```toml
  name = "Microsoft ODBC Driver 18 for SQL Server"
  version = "18.7.1.1"
  used_by = "sqlserver-odbc"
  license = "https://aka.ms/odbc18eula"
  directory = "odbc-drivers/msodbcsql18"

  [files]
  "opt/microsoft/msodbcsql18/lib64/libmsodbcsql-18.7.so.1.1" = "lib/msodbcsql18"
  "opt/microsoft/msodbcsql18/share" = "share"
  "usr/share/doc/msodbcsql18/LICENSE.txt" = "LICENSE.txt"

  [[downloads]]
  format = "deb"
  url = "https://packages.microsoft.com/debian/12/prod/pool/main/m/msodbcsql18/msodbcsql18_18.7.1.1-1_arm64.deb"
  sha256 = "6a4cafb5efe6b899aeb674de56fdceee2380930f1a46c3693ba70f41291c81eb"

  [[downloads]]
  format = "rpm"
  url = "https://packages.microsoft.com/rhel/8/prod/Packages/m/msodbcsql18-18.7.1.1-1.aarch64.rpm"
  sha256 = "3bd510518be3eaa87e6b21fa3d74e6c469ac4b1e71b0e254acff47fc46dd89b1"
  ```

  A new format is one more unpacking case in `fetch-dependencies` and in the
  formats `package.sh` accepts.

### Kinds

| Kind | Placed in | Fields |
| --- | --- | --- |
| `adbc-driver` | `adbc-drivers/<name>.toml` and the library it names, relative to it; license texts in `LICENSES/<name>/` | `name`, `version`, `library` and `licenses` (paths in the wheel), `wheels.<platform>.url` and `.hash` (SRI sha256) |
| `adbc-driver-archive` | As `adbc-driver`, from a per-platform tar.gz archive instead of a wheel, such as an ADBC Driver Foundry release | `name`, `version`, `licenses` (file paths in the archive), `archives.<platform>.url`, `.hash` (SRI sha256), and `.library` (path in the archive) |
| `prebuilt` | The libraries under `lib` in `directory`, `lib/` by default; the installation prefix on the DuckDB extension build's `CMAKE_PREFIX_PATH`; the artifact's `LICENSES/` (license texts and `SOURCES.txt`) in `LICENSES/<name>/` ([Prebuilt dependencies](#prebuilt-dependencies)) | `version` and optional `revision` (naming the release), `lib` (file names in the artifact's `lib/`), optional `directory` (in the package), and `artifacts.<platform>` (the artifact's SRI sha256) |
| `odbc-driver` | `downloaded` only: `odbc-drivers/<name>/`, where `files` places the driver at the path its connector names; the manifest in `downloads/<name>.toml` | Those of every `downloaded` entry ([Delivery](#delivery)) |

### Source code availability

An entry that contains a component whose license requires its source code to
be available with a binary, such as MPL-2.0, lists that component under
`sources`, each with `name`, `version`, `license`, and the `url` of its source
code at that version. `package.sh` writes the list to
`LICENSES/<name>/SOURCES.txt`. A `prebuilt` entry has no `sources`: its
artifact carries its own `SOURCES.txt`, naming the upstream sources it was
built from, which its release holds beside the artifacts
([Prebuilt dependencies](#prebuilt-dependencies)).

The versions are those built into the shipped binary, so they are read again
whenever the entry's version changes: for a driver built from Go, from the
module build information the binary embeds (`go version -m <library>`).

## Adding a connector

1. If the connector ships dependencies beyond the library, add its
   `connector-api` feature under `connectors`, with those dependencies. Its
   feature alone builds it into the library.
2. Declare each new dependency under `dependencies` with an existing kind, or
   add a kind as below.
3. Build and check the package on each platform. `test/check-package.sh`
   rejects a shipped binary that references a library which is neither a
   system library nor in the package's `lib/` or the binary's own directory:
   such a library is a dependency
   that is not declared, or is placed where nothing finds it.
4. If what the connector ships needs checking in the package itself, such as
   a downloaded driver and the libraries it loads, add its package test,
   `crates/connectors/<connector>/tests/package/e2e.sh`.
   `test/check-package.sh` runs it with the unpacked package, after fetching
   its dependencies, and the DuckDB CLI: `e2e.sh <package> <DuckDB CLI>`. The
   test exits without output when the package does not carry the connector,
   decides itself where its Data Source can run, and prints its own TAP lines
   (`ok - ...`, `not ok - ...`, or `ok - ... # SKIP <reason>`).

## Adding a kind

A kind is a function `ship_<kind>` in `package.sh`, with `-` in the kind
written as `_`. It runs once per dependency of that kind, with `$dependency`
set to the entry's key; `field '<jq path>'` reads the entry and `fetch` downloads
a URL checked against its hash. It places the dependency in the staged package
(`$stage`) and copies its license texts into `$stage/LICENSES/`.

Where a kind places its files depends on what finds them:

- A native library the connector API library links goes to `lib/`. The library
  searches its own directory (`@loader_path`, `$ORIGIN`), so the library finds
  it there. Its own references to other non-system libraries must point to
  `lib/` as well, which the reference check verifies.
- A file the library loads by location, such as an ADBC or ODBC driver, goes
  to the directory the library is told, as `adbc-drivers/` and `odbc-drivers/`
  are told by the DuckDB extension. A new such location is a library setting,
  which changes the C ABI.
- A library that an ODBC driver needs beyond the system goes beside the driver,
  in `odbc-drivers/<driver>/lib/`, and the connector declares it among its
  driver libraries (`OdbcConnector::DRIVER_LIBRARIES`), which `connector-odbc`
  loads before the driver.

A kind delivered as `downloaded` is instead a function `download_<kind>` that
calls `download <directory>` with the package directory the kind places it in;
`download` writes the dependency's manifest in `downloads/`, and
`package.sh` puts `fetch-dependencies` in `bin/` with it.

Add the kind to the table above.

## Prebuilt dependencies

A dependency is taken as its upstream's official prebuilt binaries when the
upstream publishes them, as the ADBC drivers are. A dependency whose upstream
distributes only sources, such as protobuf and abseil, unixODBC, or OpenSSL
and MIT Kerberos, is built from those unmodified sources once, by
[scalar-labs/scalardb-analytics-third-party-deps](https://github.com/scalar-labs/scalardb-analytics-third-party-deps),
which publishes the artifacts with the sources as GitHub Releases. No build
here builds such a dependency: `package.sh` fetches the artifact of the
release `<dependency>-<version>[-r<revision>]` and checks it against the hash
declared under `artifacts`, and the development shell (`flake.nix`) takes the
same artifact of protobuf. Third-party builds (distribution packages,
conda-forge) are not used.

To change such a dependency's version or build, release it in that
repository (its README, Releasing a dependency), then set the entry's
`version` and `revision` to the release and `artifacts` to the hashes its
publish step prints: the SHA-256 that the release's `<artifact>.tar.gz.sha256`
holds, in SRI form (`sha256-<base64>`).

`package.sh` takes an artifact from `target/prebuilt/` before downloading it,
still checked against its declared hash. A local build of that repository
differs from the published artifact byte for byte, so it is used only with its
own hash declared, to try a recipe change before it is released.

| Entry | Components | Platforms | Used by |
| --- | --- | --- | --- |
| `protobuf` | abseil, protobuf (static, with protoc) | osx_arm64, linux_amd64, linux_arm64 | The DuckDB extension, which links them statically, in the package and the development shell |
| `unixodbc` | unixODBC (libodbc, libodbcinst) | linux_amd64, linux_arm64 | `sqlserver-odbc`: the ODBC driver manager its library links, in `lib/` |
| `kerberos` | OpenSSL 3, MIT Kerberos (without keyutils) | linux_amd64, linux_arm64 | `sqlserver-odbc`: what Microsoft ODBC Driver 18 needs beyond the system, beside the driver in `odbc-drivers/msodbcsql18/lib/` |
