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
          # shared library with libpq and OpenSSL linked statically.
          adbcDriverPostgresqlWheels = {
            "aarch64-darwin" = {
              url = "https://files.pythonhosted.org/packages/04/2f/79e0e9b471293320a79805a6dd77199ceaf8d73789dce35af5959b2db778/adbc_driver_postgresql-1.13.0-py3-none-macosx_11_0_arm64.whl";
              hash = "sha256-eYGufF7Ne6TGSc0AqW6Bzw45R10/pHKQHF/Rm06d5Kg=";
            };
            "x86_64-darwin" = {
              url = "https://files.pythonhosted.org/packages/25/3c/9f62b28e0a61e0af885e5e7271a78fc899dac3e9641c42aab01f7fe716bb/adbc_driver_postgresql-1.13.0-py3-none-macosx_10_15_x86_64.whl";
              hash = "sha256-Ev1kv27BZwXTDSbDisLMR/6McAasKQLEMjTWHLauikM=";
            };
            "aarch64-linux" = {
              url = "https://files.pythonhosted.org/packages/a8/07/3c6c59a60fb423df8e5912f6442c1c2a0a966d5a0b8dc82de0807b8256d2/adbc_driver_postgresql-1.13.0-py3-none-manylinux_2_26_aarch64.manylinux_2_28_aarch64.whl";
              hash = "sha256-8vPFEHb8J7p2i60txMw4iGVaxxlHOqfZM7KBOGBJEBQ=";
            };
            "x86_64-linux" = {
              url = "https://files.pythonhosted.org/packages/37/6b/dd22c016305e84b8b9959784e77bbd20c454bd82463dc8de5a839abde206/adbc_driver_postgresql-1.13.0-py3-none-manylinux_2_26_x86_64.manylinux_2_28_x86_64.whl";
              hash = "sha256-qGgZunx5sdBnxKU0Q1i000a6tY6IjXh57Ssl2XzFm6E=";
            };
          };
          adbcDriverPostgresql = pkgs.stdenv.mkDerivation {
            pname = "adbc-driver-postgresql";
            version = "1.13.0";
            src = pkgs.fetchurl adbcDriverPostgresqlWheels.${system};
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
            # connectors load the driver and learn its version.
            installPhase = ''
              install -Dm755 adbc_driver_postgresql/libadbc_driver_postgresql.so \
                $out/lib/libadbc_driver_postgresql.so
              cat > $out/lib/adbc_driver_postgresql.toml <<EOF
              manifest_version = 1
              name = "ADBC PostgreSQL Driver"
              version = "1.13.0"

              [Driver]
              shared = "$out/lib/libadbc_driver_postgresql.so"
              EOF
            '';
          };
          # Directory of the ADBC drivers that connector tests load.
          adbcDriverDirectory = "${adbcDriverPostgresql}/lib";
          # The ODBC driver manager that the ODBC ADBC driver links, and the
          # PostgreSQL ODBC driver that its tests reach a server through.
          odbcLibraryPath = pkgs.lib.makeLibraryPath [ pkgs.unixodbc ];
          psqlOdbcDriver = "${pkgs.unixodbcDrivers.psql}/lib/psqlodbcw.so";
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
          packages.adbc-driver-postgresql = adbcDriverPostgresql;

          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ devshell.overlays.default ];
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
                name = "PSQLODBC_DRIVER";
                value = psqlOdbcDriver;
              }
            ]
            # Linux libraries carry no install path, so the ODBC driver
            # manager is found at run time through the search path.
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
              {
                name = "LD_LIBRARY_PATH";
                value = odbcLibraryPath;
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
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          };

          devShells.msrv = pkgs.mkShellNoCC {
            name = "scalardb-analytics-connectors-msrv";
            shellHook = ''
              export ADBC_DRIVER_DIRECTORY="${adbcDriverDirectory}"
              export LIBRARY_PATH="${libraryPath}"
              export PSQLODBC_DRIVER="${psqlOdbcDriver}"
            ''
            + pkgs.lib.optionalString pkgs.stdenv.isLinux ''
              export LD_LIBRARY_PATH="${odbcLibraryPath}"
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
