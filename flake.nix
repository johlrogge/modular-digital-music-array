{
  description = "Music Player NixOS Configuration";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-23.11";
    nixos-hardware.url = "github:nixos/nixos-hardware";
  };

  outputs = { self, nixpkgs, nixos-hardware }: {
    # Development VM configuration (x86_64)
    nixosConfigurations.music-player-vm = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ({ config, pkgs, ... }: {
          # Basic system configuration
          system.stateVersion = "23.11";
          
          # Sound Configuration
          sound.enable = true;

          # System Services
          services = {
            # PipeWire Configuration
            pipewire = {
              enable = true;
              alsa.enable = true;
              alsa.support32Bit = false;
              pulse.enable = true;
            };
          };
          
          # PipeWire custom configuration
          environment.etc."pipewire/pipewire.conf.d/92-low-latency.conf".text = ''
            context.properties = {
                default.clock.rate = 48000
                default.clock.quantum = 1024
                default.clock.min-quantum = 32
                default.clock.max-quantum = 8192
            }
          '';

          # VM-specific settings
          virtualisation.vmVariant = {
            virtualisation = {
              graphics = false;
              cores = 4;
              memorySize = 8192;
              diskSize = 32768;
            };
          };

          # System Packages
          environment.systemPackages = with pkgs; [
            pipewire
            alsa-utils
            vim
            git
            htop
            rustup
          ];

          # User Configuration
          users.users.music = {
            isNormalUser = true;
            extraGroups = [ "audio" "pipewire" "wheel" ];
            initialPassword = "changeme";
          };

          # System Optimization
          systemd.services.pipewire = {
            serviceConfig = {
              Nice = -11;
              IOSchedulingClass = "realtime";
              IOSchedulingPriority = 0;
              CPUSchedulingPolicy = "fifo";
              CPUSchedulingPriority = 99;
            };
          };
        })
      ];
    };

    # Raspberry Pi deployment configuration (aarch64)
    nixosConfigurations.music-player-pi = nixpkgs.lib.nixosSystem {
      system = "aarch64-linux";
      modules = [
        ./configuration.nix
        nixos-hardware.nixosModules.raspberry-pi-5
      ];
    };

    # Development VM package
    packages.x86_64-linux.default = self.nixosConfigurations.music-player-vm.config.system.build.vm;
  };
}
