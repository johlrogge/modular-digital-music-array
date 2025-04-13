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

        # Use pkgs.lib instead of lib directly
        commonEnv = {
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.llvmPackages.libclang.dev}/include -I${pkgs.pipewire.dev}/include";
  
          # Add explicit paths to headers, which are necessary for nng's cmake build
          NIX_CFLAGS_COMPILE = "-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.gcc}/lib/gcc/${pkgs.stdenv.hostPlatform.config}/${pkgs.gcc.version}/include";
          CMAKE_C_FLAGS = "-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.gcc}/lib/gcc/${pkgs.stdenv.hostPlatform.config}/${pkgs.gcc.version}/include";
          CMAKE_CXX_FLAGS = "-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.gcc}/lib/gcc/${pkgs.stdenv.hostPlatform.config}/${pkgs.gcc.version}/include";
        };

        # Common dependencies for both build and dev shell
        commonDeps = with pkgs; [
          alsa-lib
          alsa-plugins
          pipewire
          llvmPackages.libclang
          # Add NNG if available in nixpkgs
          nng
        ];

        commonNativeDeps = with pkgs; [
          pkg-config
          cmake
          llvmPackages.clang
          ffmpeg
          # Add the following packages:
          glibc.dev
          gcc
          stdenv.cc.cc.lib
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

        devShells.default = pkgs.mkShell {
          # For build-time dependencies that provide tools
          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
            rustc
            cargo
          ];
  
          # For runtime and compile-time library dependencies
          buildInputs = with pkgs; [
            # Add the standard C development tools - this is the important part!
            stdenv.cc.cc.lib
            glibc
            glibc.dev
            # Other dependencies
            alsa-lib
            pipewire
          ];
  
          # Only set truly necessary environment variables
          shellHook = ''
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            # This is what actually fixes your headers issue:
            export NIX_CFLAGS_COMPILE="-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.stdenv.cc.cc}/lib/gcc/${pkgs.stdenv.targetPlatform.config}/${pkgs.stdenv.cc.version}/include"
          '';
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
