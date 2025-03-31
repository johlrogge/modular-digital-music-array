{
  description = "Modular Distributed Music Array";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.11";
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

        # Shared environment variables for both build and dev shell
        commonEnv = {
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.llvmPackages.libclang.dev}/include -I${pkgs.pipewire.dev}/include";
        };

        # Common dependencies for both build and dev shell
        commonDeps = with pkgs; [
          alsa-lib
          alsa-plugins
          pipewire
          llvmPackages.libclang
        ];

        commonNativeDeps = with pkgs; [
          pkg-config
          cmake
          llvmPackages.clang
          ffmpeg
        ];

        commonArgs = {
          src = craneLib.cleanCargoSource ./.;
          pname = "download-cli";
          version = "0.1.0";
          
          buildInputs = commonDeps;
          nativeBuildInputs = commonNativeDeps;
          
          # Pass environment variables to the build
          inherit (commonEnv) LIBCLANG_PATH;
          inherit (commonEnv) BINDGEN_EXTRA_CLANG_ARGS;
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
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.yt-dlp ]} \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath commonDeps}
          '';
        };
      in
      {
        packages = {
          inherit download-cli-wrapped;
          default = download-cli-wrapped;
        };

        devShells.default = pkgs.mkShell ({
          packages = with pkgs; [
            rustToolchain
            yt-dlp
          ] ++ commonDeps ++ commonNativeDeps;
          
          inherit (commonEnv) LIBCLANG_PATH;
          inherit (commonEnv) BINDGEN_EXTRA_CLANG_ARGS;
          
          LD_LIBRARY_PATH = with pkgs; lib.makeLibraryPath commonDeps;
          ALSA_PLUGIN_DIR = "${pkgs.alsa-plugins}/lib/alsa-lib";
        });

        nixosConfigurations.default = nixpkgs.lib.nixosSystem {
          inherit system;
          modules = [
            ./configuration.nix
          ];
        };
      }
    );
}
