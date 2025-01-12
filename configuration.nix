# Edit this configuration file to define what should be installed on
# your system. Help is available in the configuration.nix(5) man page
# and in the NixOS manual (accessible by running 'nixos-help').

{ config, pkgs, ... }:

{
  # System Configuration
  system.stateVersion = "23.11"; # Please read the documentation before changing

  # Hardware Configuration will be provided by nixos-hardware module

  # Sound Configuration
  sound.enable = true;

  # Boot Configuration
  boot = {
    loader = {
      generic-extlinux-compatible.enable = true;
      raspberryPi = {
        enable = true;
        version = 5;
      };
    };
    
    # Kernel Parameters
    kernelParams = [
      "console=ttyAMA0,115200"
      "console=tty1"
      # Audio related tweaks
      "threadirqs"
      "isolcpus=3" # Isolate last CPU core for audio processing
    ];
    
    # Kernel Modules
    kernelModules = [ "snd-usb-audio" ];
  };

  # Storage Configuration
  fileSystems = {
    "/" = {
      device = "/dev/disk/by-label/NIXOS_SD";
      fsType = "ext4";
      options = [ "noatime" "nodiratime" ];
    };
    
    "/boot" = {
      device = "/dev/disk/by-label/NIXOS_BOOT";
      fsType = "vfat";
    };
    
    "/data/music" = {
      device = "/dev/disk/by-label/MUSIC_LIB";
      fsType = "ext4";
      options = [ "noatime" "nodiratime" ];
    };
    
    "/data/cache" = {
      device = "/dev/disk/by-label/MUSIC_CACHE";
      fsType = "ext4";
      options = [ "noatime" "nodiratime" ];
    };
    
    "/data/backups" = {
      device = "/dev/disk/by-label/MUSIC_BACKUP";
      fsType = "ext4";
      options = [ "noatime" "nodiratime" ];
    };
  };

  # Networking
  networking = {
    hostName = "music-player";
    networkmanager.enable = true;
    firewall = {
      enable = true;
      allowedTCPPorts = [ 
        1780  # Snapcast control
        4953  # OSC timing
      ];
    };
  };

  # System Services
  services = {
    # PipeWire Configuration
    pipewire = {
      enable = true;
      alsa.enable = true;
      alsa.support32Bit = false;
      pulse.enable = true;
      
      # Basic PipeWire setup, configuration is in /etc/pipewire/pipewire.conf.d/
    };

    # Enable OpenSSH for remote management
    openssh.enable = true;
  };

  # System Packages
  environment.systemPackages = with pkgs; [
    # Audio utilities
    pipewire
    alsa-utils
    # System utilities
    vim
    git
    htop
    # Development tools
    rustup
  ];

  # User Configuration
  users.users.music = {
    isNormalUser = true;
    extraGroups = [ "audio" "pipewire" "networkmanager" "wheel" ];
    initialPassword = "changeme";
  };

  # System Optimization
  systemd = {
    # Optimize services for audio
    user.extraConfig = ''
      DefaultCPUAccounting=yes
      DefaultIOAccounting=yes
    '';
    services = {
      pipewire = {
        serviceConfig = {
          Nice = -11;
          IOSchedulingClass = "realtime";
          IOSchedulingPriority = 0;
          CPUSchedulingPolicy = "fifo";
          CPUSchedulingPriority = 99;
        };
      };
    };
  };

  # Performance Tuning
  boot.kernel.sysctl = {
    # IO optimizations
    "vm.swappiness" = 10;
    # Audio optimizations
    "fs.inotify.max_user_watches" = 524288;
    # Network optimizations
    "net.core.rmem_max" = 2500000;
    "net.core.wmem_max" = 2500000;
  };
}
