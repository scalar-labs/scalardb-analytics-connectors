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
          # The Oracle Instant Client ODBC driver, which the Oracle
          # connector's tests use: Instant Client Basic and its ODBC package
          # in one directory, with `libsqora` linking to the driver, so that
          # the driver's path is the same on every platform. The driver finds
          # the client libraries beside itself. nixpkgs pins a different
          # Instant Client version per platform, and the driver's file name
          # carries the major version, such as `libsqora.so.21.1`.
          oracleInstantClient = pkgs.oracle-instantclient;
          oracleOdbcDriver = pkgs.runCommand "oracle-odbc-driver" { } ''
            mkdir -p $out
            ln -s ${oracleInstantClient.lib}/lib/* $out/
            ln -s ${oracleInstantClient.lib}/lib/libsqora${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}.${pkgs.lib.versions.major oracleInstantClient.version}.1 $out/libsqora
            test -e $out/libsqora
          '';
          # Directory of the ODBC drivers that connector tests load, each at
          # the path its Connector names: the PostgreSQL ODBC driver that
          # connector-odbc's tests reach a server through, the Microsoft
          # ODBC Driver 18 for SQL Server that the SQL Server through ODBC
          # connector's tests use, with its `share` beside its `lib` because
          # it loads its resources relative to its own path, and the Oracle
          # Instant Client ODBC driver.
          msodbcsql = pkgs.unixodbcDrivers.msodbcsql18;
          odbcDriverDirectory = pkgs.linkFarm "odbc-drivers" {
            psqlodbcw = "${pkgs.unixodbcDrivers.psql}/lib/psqlodbcw.so";
            "msodbcsql18/lib/msodbcsql18" = "${msodbcsql}/${msodbcsql.driver}";
            "msodbcsql18/share" = "${msodbcsql}/share";
            instantclient = oracleOdbcDriver;
          };
          # Libraries the ODBC drivers load by file name, which on macOS they
          # find only through `DYLD_LIBRARY_PATH`: OpenSSL, which the SQL
          # Server driver loads at run time, and unixODBC's installer library,
          # which the Oracle driver links through its `@rpath`.
          odbcDriverLibraryPath = pkgs.lib.makeLibraryPath [
            pkgs.openssl
            pkgs.unixodbc
          ];
          # Libraries the workspace links: the ODBC driver manager, and on
          # macOS libiconv.
          libraryPath = pkgs.lib.makeLibraryPath (
            [ pkgs.unixodbc ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ]
          );
          # DuckDB v1.5.6, the version the DuckDB adapter targets, from its
          # release files instead of a source build: nixpkgs does not provide
          # this version, so no binary cache holds a build of it. The layout
          # gives the adapter's CMake build what the distribution build takes:
          #
          #   source/  the C++ headers and the extension loader source from the
          #            source archive
          #   static/  the static libraries the loadable extension carries
          #   lib/     the shared library the adapter's tests link
          #   bin/     the CLI the end-to-end test loads the extension into
          duckdbAssets = {
            "aarch64-darwin" = {
              static = "osx-arm64";
              library = "osx-universal";
              cli = "osx-arm64";
            };
            "x86_64-darwin" = {
              static = "osx-amd64";
              library = "osx-universal";
              cli = "osx-amd64";
            };
            "aarch64-linux" = {
              static = "linux-arm64";
              library = "linux-arm64";
              cli = "linux-arm64";
            };
            "x86_64-linux" = {
              static = "linux-amd64";
              library = "linux-amd64";
              cli = "linux-amd64";
            };
          }.${system};
          duckdbSha256 = {
            "static-libs-osx-arm64.zip" = "e77c382fba15e3f5c0b3265d12e3a4de534699fdb91bcc9d10d4a857a58103d5";
            "static-libs-osx-amd64.zip" = "ff1d5092d4a495e9da831c3a2de5f2ee37b5f87ae23ec7e9aa7adb434bd8a134";
            "static-libs-linux-arm64.zip" = "56249751117a4c04289764e06eb431352ce33fb3f8e2f30401122d4d0e184139";
            "static-libs-linux-amd64.zip" = "ab3d1d33951b8cb8bdcae740d8e06adf66bfb7ee880a3f7052d7a35088e16d2d";
            "libduckdb-osx-universal.zip" = "e0bc007d9b0094c0970ac1847a8601d10aad07cbd2910ce586ec810f77b638d6";
            "libduckdb-linux-arm64.zip" = "b72ed9f05003f5e9d2015f7ceada6416b377d9dd33169cdde3c9e33897856eee";
            "libduckdb-linux-amd64.zip" = "b845005f5132a7d8180057c35e14a7626632258782f871a90861b19c1c03841b";
            "duckdb_cli-osx-arm64.zip" = "8e0f6825653f8d057922e6147db920bebf072cb41f4b041fd35521c18d7d126e";
            "duckdb_cli-osx-amd64.zip" = "ae74c8cd74304bde1d92d941aca37bc72084ee8daa5660913819d8a9e9d29331";
            "duckdb_cli-linux-arm64.zip" = "c544e92c9b7c31fc53c2139802cabd8e2d1b2b3e3f933117f31611239c1402db";
            "duckdb_cli-linux-amd64.zip" = "6e89deac1ebbc36eed0291caf8b567b030c7b86ac35998f71854e22b3c5d5e2f";
          };
          duckdbAsset =
            name:
            pkgs.fetchurl {
              url = "https://github.com/duckdb/duckdb/releases/download/v1.5.6/${name}";
              sha256 = duckdbSha256.${name};
            };
          duckdbSource = pkgs.fetchurl {
            url = "https://github.com/duckdb/duckdb/archive/refs/tags/v1.5.6.tar.gz";
            sha256 = "1fadcbe9e69e1470f9093b6bcde08daf477d729c449e59a807f45c346622099b";
          };
          duckdb = pkgs.stdenv.mkDerivation {
            pname = "duckdb-release";
            version = "1.5.6";
            dontUnpack = true;
            # The manylinux binaries need libstdc++ but carry no RUNPATH, and
            # the CLI names the host's dynamic loader.
            nativeBuildInputs = [
              pkgs.unzip
            ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
            buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc.cc.lib ];
            # The release files are used as shipped, apart from the Linux
            # binaries' library lookup.
            dontStrip = true;
            dontFixup = pkgs.stdenv.isDarwin;
            installPhase = ''
              mkdir -p $out/source $out/static $out/lib $out/bin
              tar -xzf ${duckdbSource} -C $out/source --strip-components=1 \
                duckdb-1.5.6/src/include \
                duckdb-1.5.6/extension/loader/dummy_static_extension_loader.cpp
              unzip -q ${duckdbAsset "static-libs-${duckdbAssets.static}.zip"} -d $out/static
              unzip -q ${duckdbAsset "libduckdb-${duckdbAssets.library}.zip"} 'libduckdb.*' -d $out/lib
              unzip -q ${duckdbAsset "duckdb_cli-${duckdbAssets.cli}.zip"} -d $out/bin
            '';
          };
          # The Substrait protobuf definitions of the specification version that
          # the Rust `substrait` crate pins; the DuckDB adapter generates its
          # C++ Substrait types from them.
          substraitProto = pkgs.fetchzip {
            url = "https://github.com/substrait-io/substrait/archive/refs/tags/v0.69.0.tar.gz";
            sha256 = "0ga2frvw9w22d8axamlb08b01piy44p3lphbnk9102vrymh5mgn6";
          };
          # Static protobuf and abseil for the DuckDB adapter's CMake build. The
          # loadable extension carries its own copy behind its export
          # restriction, as the distribution build's does: generated Substrait
          # code registers its descriptors in the protobuf it links when it is
          # loaded, and a second registration in one shared protobuf aborts.
          # `abseil-cpp` only names `abseil-cpp_202601`, which takes the option.
          #
          # Unlike DuckDB, these are built from source. No binary cache holds
          # the static variants, and neither project publishes static
          # libraries: protobuf releases only protoc, and conda-forge's static
          # protobuf links a shared abseil. CI keeps the build in its Nix store
          # cache, and a build without the cache takes a few minutes.
          abseilStatic = pkgs.abseil-cpp_202601.override { static = true; };
          # protobuf's own tests are not run: no binary cache holds this
          # variant, and one of them fails to write its temporary file in the
          # macOS Nix builder. It is built without zlib, as the distribution
          # build's is: the static library would otherwise require consumers
          # to find a zlib, which the development shell does not provide.
          protobufStatic =
            (pkgs.protobuf.override {
              enableShared = false;
              abseil-cpp = abseilStatic;
            }).overrideAttrs
              (previousAttrs: {
                cmakeFlags = previousAttrs.cmakeFlags ++ [ "-Dprotobuf_WITH_ZLIB=OFF" ];
                doCheck = false;
              });
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
            ];
            # The ODBC drivers of SQL Server and Oracle are the only unfree
            # packages allowed.
            config.allowUnfreePredicate =
              pkg:
              builtins.elem (pkgs.lib.getName pkg) [
                "msodbcsql18"
                "oracle-instantclient"
              ];
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
                name = "DUCKDB_DIR";
                value = "${duckdb}";
              }
              {
                name = "CMAKE_PREFIX_PATH";
                value = pkgs.lib.makeSearchPath "" [
                  protobufStatic
                  (pkgs.lib.getDev abseilStatic)
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
                value = odbcDriverLibraryPath;
              }
            ];
            packages = [
              rustToolchain
              pkgs.clang
              pkgs.protobuf
              pkgs.cmake
              pkgs.ninja
              pkgs.pkg-config
              duckdb
              pkgs.unixodbc
              # The distribution package script (packaging/package.sh).
              pkgs.cargo-about
              pkgs.jq
              pkgs.unzip
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          };

          # What the distribution package script (packaging/package.sh) needs
          # on macOS, without the DuckDB adapter's development inputs: the
          # distribution build takes DuckDB, protobuf, and abseil from pinned
          # release files and sources itself.
          devShells.package = pkgs.mkShellNoCC {
            name = "scalardb-analytics-connectors-package";
            shellHook = ''
              export LIBRARY_PATH="${libraryPath}"
            '';
            packages = [
              rustToolchain
              pkgs.clang
              pkgs.protobuf
              pkgs.cmake
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
              export DYLD_LIBRARY_PATH="${odbcDriverLibraryPath}"
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
