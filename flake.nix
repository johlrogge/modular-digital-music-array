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
    let
      # System-specific outputs (packages, devShell)
      perSystem = flake-utils.lib.eachDefaultSystem (system:
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
            
            buildInputs = [];
            nativeBuildInputs = [ pkgs.pkg-config ];
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
              pipewire
            ];
          };
        }
      );

      # NixOS configuration for the VM
      nixosConfig = { config, lib, pkgs, ... }: {
        imports = [
          ./configuration.nix  # Import the existing NixOS configuration
        ];

        # VM-specific settings
        virtualisation = {
          cores = 2;
          memorySize = 4096; # MB
          graphics = true;   # Enable graphical output
        };

        # Auto-start services
        systemd.services.mdma = {
          description = "Modular Distributed Music Array";
          wantedBy = [ "multi-user.target" ];
          after = [ "network.target" "pipewire.service" ];
          
          serviceConfig = {
            Type = "simple";
            User = "music";
            ExecStart = "${self.packages.${pkgs.system}.download-cli-wrapped}/bin/download-cli";
            Restart = "on-failure";
          };
        };
      };
    in
    {
      # Merge the per-system outputs
      inherit (perSystem) packages devShells;

      # Add NixOS configuration
      nixosConfigurations.default = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";  # You might want to make this configurable
        modules = [
          nixosConfig
        ];
      };
    };
}
