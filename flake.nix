{
  description = "Development environment for ScalarDB Analytics Connector";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0";
    flake-parts.url = "github:hercules-ci/flake-parts";
    devshell = {
      url = "github:numtide/devshell";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      nixpkgs,
      flake-parts,
      devshell,
      fenix,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ ];

      flake = { };

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];

      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          system,
          ...
        }:
        let
          rustToolchain = fenix.packages.${system}.stable.withComponents [
            "cargo"
            "clippy"
            "rust-analyzer"
            "rust-src"
            "rustc"
            "rustfmt"
          ];
          msrvToolchain = (fenix.packages.${system}.toolchainOf {
            channel = "1.88.0";
            sha256 = "sha256-Qxt8XAuaUR2OMdKbN4u8dBJOhSHxS+uS06Wl9+flVEk=";
          }).withComponents [
            "cargo"
            "rustc"
          ];
          # The Apache Arrow ADBC PostgreSQL driver that the PostgreSQL
          # connector's tests load. The PyPI wheel ships one self-contained
          # shared library with libpq and OpenSSL linked statically; the
          # distribution package ships the same wheels (packaging/dependencies.json).
          adbcDriverPostgresqlPin =
            (builtins.fromJSON (builtins.readFile ./packaging/dependencies.json))
            .dependencies.adbc_driver_postgresql;
          duckdbPlatform = {
            "aarch64-darwin" = "osx_arm64";
            "x86_64-darwin" = "osx_amd64";
            "aarch64-linux" = "linux_arm64";
            "x86_64-linux" = "linux_amd64";
          }.${system};
          adbcDriverPostgresql = pkgs.stdenv.mkDerivation {
            pname = "adbc-driver-postgresql";
            version = adbcDriverPostgresqlPin.version;
            src = pkgs.fetchurl adbcDriverPostgresqlPin.wheels.${duckdbPlatform};
            # The manylinux library needs libstdc++ but carries no RUNPATH, and
            # Nix-built processes do not search the host's library directories.
            nativeBuildInputs = [
              pkgs.unzip
            ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
            buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc.cc.lib ];
            unpackPhase = "unzip -q $src";
            dontBuild = true;
            # The macOS library is used as shipped.
            dontFixup = pkgs.stdenv.isDarwin;
            # Install the library with an ADBC driver manifest, through which
            # connectors load the driver and learn its version. The manifest
            # names the library relative to itself, as in the distribution
            # package.
            installPhase = ''
              install -Dm755 adbc_driver_postgresql/libadbc_driver_postgresql.so \
                $out/lib/libadbc_driver_postgresql.so
              cat > $out/lib/adbc_driver_postgresql.toml <<EOF
              manifest_version = 1
              name = "${adbcDriverPostgresqlPin.name}"
              version = "${adbcDriverPostgresqlPin.version}"

              [Driver]
              shared = "libadbc_driver_postgresql.so"
              EOF
            '';
          };
          # The ADBC Driver Foundry MySQL driver that the MySQL connector's
          # tests load. Its tarball ships one self-contained shared library;
          # no build is published for x86_64-darwin.
          adbcDriverMysqlTarballs = {
            "aarch64-darwin" = {
              url = "https://dbc-cdn.columnar.tech/mysql/v0.8.0/mysql_macos_arm64_v0.8.0.tar.gz";
              hash = "sha256-Lt9uyMQilEbrbV7Zi6ugOe0N3rLllBsx7rKAgWCI9PM=";
            };
            "aarch64-linux" = {
              url = "https://dbc-cdn.columnar.tech/mysql/v0.8.0/mysql_linux_arm64_v0.8.0.tar.gz";
              hash = "sha256-AQGrc+zFjzxAEW5egx4Jwce8ugU1XYbc9xYDSZTD9Us=";
            };
            "x86_64-linux" = {
              url = "https://dbc-cdn.columnar.tech/mysql/v0.8.0/mysql_linux_amd64_v0.8.0.tar.gz";
              hash = "sha256-uL72uaQxskbpLB+ed7R94y2/quh9vAjhSmIwiNqkY48=";
            };
          };
          adbcDriverMysqlLibrary =
            if pkgs.stdenv.isDarwin then "libadbc_driver_mysql.dylib" else "libadbc_driver_mysql.so";
          adbcDriverMysql = pkgs.stdenv.mkDerivation {
            pname = "adbc-driver-mysql";
            version = "0.8.0";
            src = pkgs.fetchurl adbcDriverMysqlTarballs.${system};
            sourceRoot = ".";
            dontBuild = true;
            dontFixup = true;
            # Install the library with an ADBC driver manifest, through which
            # connectors load the driver.
            installPhase = ''
              install -Dm755 ${adbcDriverMysqlLibrary} $out/lib/${adbcDriverMysqlLibrary}
              cat > $out/lib/adbc_driver_mysql.toml <<EOF
              manifest_version = 1
              name = "ADBC Driver Foundry Driver for MySQL"
              version = "0.8.0"

              [Driver]
              shared = "$out/lib/${adbcDriverMysqlLibrary}"
              EOF
            '';
          };
          # Directory of the ADBC drivers that connector tests load.
          adbcDriverDirectory = "${
            pkgs.symlinkJoin {
              name = "adbc-drivers";
              paths = [
                adbcDriverPostgresql
              ]
              ++ pkgs.lib.optionals (adbcDriverMysqlTarballs ? ${system}) [ adbcDriverMysql ];
            }
          }/lib";
          # The ODBC driver manager that connector-odbc links.
          odbcLibraryPath = pkgs.lib.makeLibraryPath [ pkgs.unixodbc ];
          # Directory of the ODBC drivers that connector tests load, each at
          # the path its Connector names: the PostgreSQL ODBC driver that
          # connector-odbc's tests reach a server through, and the Microsoft
          # ODBC Driver 18 for SQL Server that the SQL Server through ODBC
          # connector's tests use, with its `share` beside its `lib` because
          # it loads its resources relative to its own path.
          msodbcsql = pkgs.unixodbcDrivers.msodbcsql18;
          odbcDriverDirectory = pkgs.linkFarm "odbc-drivers" {
            psqlodbcw = "${pkgs.unixodbcDrivers.psql}/lib/psqlodbcw.so";
            "msodbcsql18/lib/msodbcsql18" = "${msodbcsql}/${msodbcsql.driver}";
            "msodbcsql18/share" = "${msodbcsql}/share";
          };
          # The driver loads OpenSSL at run time by its file name; on macOS it
          # finds it only through `DYLD_LIBRARY_PATH`, which holds nothing but
          # OpenSSL.
          msodbcsqlLibraryPath = pkgs.lib.makeLibraryPath [ pkgs.openssl ];
          # Libraries the workspace links: the ODBC driver manager, and on
          # macOS libiconv.
          libraryPath = pkgs.lib.makeLibraryPath (
            [ pkgs.unixodbc ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ]
          );
          # The Substrait protobuf definitions of the specification version that
          # the Rust `substrait` crate pins; the DuckDB adapter generates its
          # C++ Substrait types from them.
          substraitProto = pkgs.fetchzip {
            url = "https://github.com/substrait-io/substrait/archive/refs/tags/v0.69.0.tar.gz";
            sha256 = "0ga2frvw9w22d8axamlb08b01piy44p3lphbnk9102vrymh5mgn6";
          };
        in
        {
          packages = {
            adbc-driver-postgresql = adbcDriverPostgresql;
          }
          // pkgs.lib.optionalAttrs (adbcDriverMysqlTarballs ? ${system}) {
            adbc-driver-mysql = adbcDriverMysql;
          };

          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [
              devshell.overlays.default
              # The DuckDB version the adapter targets, which nixpkgs does not
              # provide yet.
              (final: prev: {
                duckdb = prev.duckdb.overrideAttrs (finalAttrs: previousAttrs: {
                  version = "1.5.6";
                  rev = "069cc9f9b5be802405797faecc284961b07c70ef";
                  src = previousAttrs.src.override {
                    tag = "v${finalAttrs.version}";
                    hash = "sha256-xHcucJA+2nTD9oeoZxDQzGS6QNnzjHQ7t9sz0OHA+zo=";
                  };
                });
              })
            ];
            # The SQL Server ODBC driver is the only unfree package allowed.
            config.allowUnfreePredicate = pkg: pkgs.lib.getName pkg == "msodbcsql18";
          };

          devShells.default = pkgs.devshell.mkShell {
            imports = [ (pkgs.devshell.importTOML ./devshell.toml) ];
            env = [
              {
                name = "ADBC_DRIVER_DIRECTORY";
                value = adbcDriverDirectory;
              }
              # Inputs of the DuckDB adapter's CMake build.
              {
                name = "SUBSTRAIT_PROTO_DIR";
                value = "${substraitProto}/proto";
              }
              {
                name = "CMAKE_PREFIX_PATH";
                value = pkgs.lib.makeSearchPath "" [
                  pkgs.duckdb.dev
                  pkgs.duckdb.lib
                  pkgs.protobuf
                  (pkgs.lib.getDev pkgs.abseil-cpp)
                  pkgs.catch2_3
                ];
              }
              {
                name = "LIBRARY_PATH";
                value = libraryPath;
              }
              {
                name = "ODBC_DRIVER_DIRECTORY";
                value = "${odbcDriverDirectory}";
              }
            ]
            # Linux libraries carry no install path, so the ODBC driver
            # manager is found at run time through the search path.
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
              {
                name = "LD_LIBRARY_PATH";
                value = odbcLibraryPath;
              }
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              {
                name = "DYLD_LIBRARY_PATH";
                value = msodbcsqlLibraryPath;
              }
            ];
            packages = [
              rustToolchain
              pkgs.clang
              pkgs.protobuf
              pkgs.cmake
              pkgs.ninja
              pkgs.pkg-config
              pkgs.duckdb
              pkgs.unixodbc
              # The distribution package script (packaging/package.sh).
              pkgs.cargo-about
              pkgs.jq
              pkgs.unzip
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          };

          devShells.msrv = pkgs.mkShellNoCC {
            name = "scalardb-analytics-connectors-msrv";
            shellHook = ''
              export ADBC_DRIVER_DIRECTORY="${adbcDriverDirectory}"
              export LIBRARY_PATH="${libraryPath}"
              export ODBC_DRIVER_DIRECTORY="${odbcDriverDirectory}"
            ''
            + pkgs.lib.optionalString pkgs.stdenv.isLinux ''
              export LD_LIBRARY_PATH="${odbcLibraryPath}"
            ''
            + pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
              export DYLD_LIBRARY_PATH="${msodbcsqlLibraryPath}"
            '';
            packages = [
              msrvToolchain
              pkgs.protobuf
              pkgs.unixodbc
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          };
        };
    };
}
