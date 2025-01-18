# flake.nix
{
  description = "Modular Distributed Music Array";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.05";  # Updated to newer nixpkgs
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        
        rustToolchain = pkgs.rust-bin.stable.latest.default;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Common arguments for both building dependencies and the final package
        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          pname = "mdma-download";
          version = "0.1.0";
          
          buildInputs = [];
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        # Build dependencies separately to improve caching
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual package
        mdma-download = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });

        # Wrap the binary with runtime dependencies
        mdma-download-wrapped = pkgs.symlinkJoin {
          name = "mdma-download";
          paths = [ mdma-download ];
          buildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            wrapProgram $out/bin/download-cli \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.yt-dlp ]}
          '';
        };
      in
      {
        packages = {
          inherit mdma-download-wrapped;
          default = mdma-download-wrapped;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            yt-dlp
            pipewire
          ];
        };
      }
    ) // {
      nixosModules.default = { config, lib, pkgs, ... }: {
        imports = [];
      };
    };
}
