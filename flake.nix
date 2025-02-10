{
  description = "Modular Distributed Music Array";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.11";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };
    crane = {
      url = "github:ipetkov/crane";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
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

        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          pname = "download-cli";
          version = "0.1.0";
          
          buildInputs = with pkgs; [
            alsa-lib
            alsa-plugins
            pipewire
          ];
          
          nativeBuildInputs = with pkgs; [ 
            pkg-config
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        download-cli = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
        });

        download-cli-wrapped = pkgs.symlinkJoin {
          name = "download-cli";
          paths = [ download-cli ];
          buildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            wrapProgram $out/bin/download-cli \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.yt-dlp ]}
          '';
        };
      in
      {
        packages = {
          inherit download-cli-wrapped;
          default = download-cli-wrapped;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustToolchain
            yt-dlp
            alsa-lib
            alsa-plugins
            alsa-utils
            pipewire
            pkg-config
          ];
          
          LD_LIBRARY_PATH = with pkgs; lib.makeLibraryPath [
            alsa-lib
            alsa-plugins
            pipewire
          ];

          # Add ALSA plugins path
          ALSA_PLUGIN_DIR = "${pkgs.alsa-plugins}/lib/alsa-lib";
        };

        nixosConfigurations.default = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            ./configuration.nix
          ];
        };
      }
    );
}