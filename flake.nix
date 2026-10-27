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
            channel = "1.85.0";
            sha256 = "sha256-AJ6LX/Q/Er9kS15bn9iflkUwcgYqRQxiOIL2ToVAXaU=";
          }).withComponents [
            "cargo"
            "rustc"
          ];
        in
        {
          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ devshell.overlays.default ];
          };

          devShells.default = pkgs.devshell.mkShell {
            imports = [ (pkgs.devshell.importTOML ./devshell.toml) ];
            env = pkgs.lib.optionals pkgs.stdenv.isDarwin [
              {
                name = "LIBRARY_PATH";
                value = pkgs.lib.makeLibraryPath [ pkgs.libiconv ];
              }
            ];
            packages = [
              rustToolchain
              pkgs.clang
              pkgs.protobuf
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          };

          devShells.msrv = pkgs.mkShellNoCC {
            name = "scalardb-analytics-connectors-msrv";
            shellHook = pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
              export LIBRARY_PATH="${pkgs.lib.makeLibraryPath [ pkgs.libiconv ]}"
            '';
            packages = [
              msrvToolchain
              pkgs.protobuf
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          };
        };
    };
}
