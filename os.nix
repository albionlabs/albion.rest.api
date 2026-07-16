{ pkgs, lib, modulesPath, docsRoot, albionEnv ? { }, ... }:

let
  inherit (import ./keys.nix) roles;

  env = {
    name = "prod";
    virtualHost = "api.albionlabs.org";
    configFile = ./config/prod.toml;
    dataDir = "/mnt/data/albion-rest-api";
    # null when the host has no attached DigitalOcean block volume (prod runs
    # on the droplet root filesystem). Non-null enables the /mnt/data mount.
    dataVolumeName = null;
  } // albionEnv;

  services = import ./services.nix;
  enabledServices = lib.filterAttrs (_: v: v.enabled) services;

  mkService = name: cfg:
    let
      path = "/nix/var/nix/profiles/per-service/${name}/bin/${cfg.bin}";
    in {
      description = "albion ${cfg.bin} (${env.name}/${name})";

      wantedBy = [ ];

      restartIfChanged = false;
      stopIfChanged = false;

      unitConfig = {
        "X-OnlyManualStart" = true;
        ConditionPathExists = "/run/albion/${name}.ready";
      };

      environment = {
        RUST_LOG =
          "albion_rest_api=info,raindex_common=info,raindex_quote=info,rocket=warn,warn";
      };

      serviceConfig = {
        User = "albion-rest-api";
        Group = "albion";
        ExecStart = "${path} serve --config /etc/albion-rest-api/config.toml";
        Restart = "always";
        RestartSec = 5;
        ReadWritePaths = [ "/mnt/data" ];
        # Leading `-` makes a missing env file non-fatal (a fresh host boots
        # before secrets are written). Supplies DRPC_API_KEY / ALCHEMY_API_KEY
        # for the additional_rpcs `${VAR}` substitution.
        EnvironmentFile = "-/etc/albion/${name}.env";
      };
    };

in {
  imports = [
    (modulesPath + "/virtualisation/digital-ocean-config.nix")
    (modulesPath + "/profiles/qemu-guest.nix")
    ./disko.nix
  ];

  boot.loader.grub = {
    efiSupport = true;
    efiInstallAsRemovable = true;
  };

  networking.useDHCP = lib.mkForce false;

  services = {
    cloud-init = {
      enable = true;
      network.enable = true;
      settings = {
        datasource_list = [ "ConfigDrive" "Digitalocean" ];
        datasource.ConfigDrive = { };
        datasource.Digitalocean = { };
        cloud_init_modules = [
          "seed_random"
          "bootcmd"
          "write_files"
          "growpart"
          "resizefs"
          "set_hostname"
          "update_hostname"
          "set_password"
        ];
        cloud_config_modules =
          [ "ssh-import-id" "keyboard" "runcmd" "disable_ec2_metadata" ];
        cloud_final_modules = [
          "write_files_deferred"
          "puppet"
          "chef"
          "ansible"
          "mcollective"
          "salt_minion"
          "reset_rmc"
          "scripts_per_once"
          "scripts_per_boot"
          "scripts_user"
          "ssh_authkey_fingerprints"
          "keys_to_console"
          "install_hotplug"
          "phone_home"
          "final_message"
        ];
      };
    };

    openssh = {
      enable = true;
      settings = {
        PasswordAuthentication = false;
        PermitRootLogin = "prohibit-password";
      };
    };

    nginx = {
      enable = true;
      recommendedTlsSettings = true;
      recommendedProxySettings = true;
      recommendedOptimisation = true;
      recommendedGzipSettings = true;

      # Rate-limit zone: 10 req/s per IP, burst 20
      appendHttpConfig = ''
        limit_req_zone $binary_remote_addr zone=api:10m rate=10r/s;
      '';

      virtualHosts.${env.virtualHost} = {
        enableACME = true;
        forceSSL = true;

        extraConfig = ''
          # Security headers
          add_header X-Content-Type-Options "nosniff" always;
          add_header X-Frame-Options "DENY" always;
          add_header Referrer-Policy "strict-origin-when-cross-origin" always;

          # Allow private registry artifact uploads while keeping request bodies bounded.
          client_max_body_size 5m;
        '';

        # Block common exploit scanners (PHP, Docker, ThinkPHP, etc.)
        locations."~* \\.(php|asp|aspx|jsp|cgi)$" = {
          return = "444";
        };
        locations."~* ^/(containers|_ignition|vendor|public/index)" = {
          return = "444";
        };

        locations."/" = {
          proxyPass = "http://127.0.0.1:8000";
          extraConfig = ''
            limit_req zone=api burst=20 nodelay;
            limit_req_status 429;
          '';
        };
      };
    };
  };

  users.users.root.openssh.authorizedKeys.keys = roles.ssh;

  security.acme = {
    acceptTerms = true;
    defaults.email = "ops@albionlabs.org";
  };

  networking.firewall = {
    enable = true;
    allowedTCPPorts = [
      22
      80
      443
    ];
  };

  # Only mount the DigitalOcean block volume when one is attached. Prod has no
  # volume (dataVolumeName = null) — data lives on the root filesystem, so the
  # tmpfiles rules below create the data dir directly.
  fileSystems = lib.mkIf (env.dataVolumeName != null) {
    "/mnt/data" = {
      device = "/dev/disk/by-id/scsi-0DO_Volume_${env.dataVolumeName}";
      fsType = "ext4";
    };
  };

  nix = {
    settings = {
      experimental-features = [ "nix-command" "flakes" ];
      auto-optimise-store = true;
      download-buffer-size = 268435456;
    };

    gc = {
      automatic = true;
      dates = "weekly";
      options = "--delete-older-than 30d";
    };
  };

  users.groups.albion = { };
  users.users."albion-rest-api" = {
    isSystemUser = true;
    group = "albion";
  };
  programs.bash.interactiveShellInit = "set -o vi";

  environment.etc."albion-rest-api/config.toml".source = env.configFile;

  services.logrotate = {
    enable = true;
    settings."${env.dataDir}/logs/*.log" = {
      su = "root albion";
      rotate = 14;
      weekly = true;
      compress = true;
      missingok = true;
      notifempty = true;
    };
  };

  systemd.tmpfiles.rules = [
    "d ${env.dataDir} 0775 root albion -"
    "d ${env.dataDir}/logs 0775 root albion -"
  ];
  systemd.services = lib.mapAttrs mkService enabledServices;

  environment.systemPackages = with pkgs; [
    bat
    curl
    htop
    sqlite
    zellij
  ];

  system.activationScripts.per-service-profiles.text =
    "mkdir -p /nix/var/nix/profiles/per-service";

  system.activationScripts.albion-docs.text = ''
    ln -sfn ${docsRoot} /var/lib/albion-docs
  '';

  system.stateVersion = "24.11";
}
