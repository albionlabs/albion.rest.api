{ deploy-rs, self }:

let
  system = "x86_64-linux";
  inherit (deploy-rs.lib.${system}) activate;
  profileBase = "/nix/var/nix/profiles/per-service";

  albionPackage = self.packages.${system}.albion-rest-api;

  services = import ./services.nix;
  enabledServices = builtins.attrNames (builtins.removeAttrs services
    (builtins.filter (n: !services.${n}.enabled)
      (builtins.attrNames services)));

  mkServiceProfile = { name, resetState ? false, dataDir ? null }:
    let
      markerFile = "/run/albion/${name}.ready";
      resetCommands = if resetState then [
        "rm -f ${dataDir}/albion.db ${dataDir}/raindex.db"
      ] else [ ];
    in activate.custom albionPackage (builtins.concatStringsSep " && " ([
      "systemctl stop ${name} || true"
      "rm -f ${markerFile}"
    ] ++ resetCommands ++ [
      "mkdir -p /run/albion"
      "touch ${markerFile}"
      "systemctl restart ${name}"
    ]));

  mkProfile = { name, resetState ? false, dataDir ? null }: {
    path = mkServiceProfile { inherit name resetState dataDir; };
    profilePath = "${profileBase}/${name}";
  };

  mkProfiles = { resetState ? false, dataDir ? null }:
    builtins.listToAttrs (map (name: {
      inherit name;
      value = mkProfile { inherit name resetState dataDir; };
    }) enabledServices);

in {
  config = {
    nodes.albion-rest-api = {
      hostname = builtins.getEnv "DEPLOY_HOST";
      sshUser = "root";
      user = "root";

      profilesOrder = [ "system" ] ++ enabledServices;

      profiles = {
        system.path =
          activate.nixos self.nixosConfigurations.albion-rest-api-prod;
      } // mkProfiles { };
    };

    # Staging on GCE. Like the staging droplet it has no Terraform state
    # entry, so the host is supplied through DEPLOY_HOST.
    nodes.albion-rest-api-staging-gce = {
      hostname = builtins.getEnv "DEPLOY_HOST";
      sshUser = "root";
      user = "root";

      profilesOrder = [ "system" ] ++ enabledServices;

      profiles = {
        system.path =
          activate.nixos self.nixosConfigurations.albion-rest-api-staging-gce;
      } // mkProfiles { };
    };

    nodes.albion-rest-api-staging = {
      hostname = builtins.getEnv "DEPLOY_HOST";
      sshUser = "root";
      user = "root";

      profilesOrder = [ "system" ] ++ enabledServices;

      profiles = {
        system.path =
          activate.nixos self.nixosConfigurations.albion-rest-api-staging;
      } // mkProfiles { };
    };
  };

  wrappers = { pkgs, infraPkgs, localSystem }:
    let
      deployInputs = infraPkgs.buildInputs
        ++ [ deploy-rs.packages.${localSystem}.deploy-rs ];

      # If DEPLOY_HOST is already set (e.g. staging, which has no terraform
      # state entry), use it directly and skip terraform IP resolution. This is
      # what makes both prod (terraform-resolved) and staging (env-provided)
      # deploys work through the same wrappers. Only `parseIdentity` runs in the
      # short-circuit path so `$identity`/`ssh_flag` are still populated.
      deployPreamble = ''
        ${infraPkgs.parseIdentity}
        if [ -n "''${DEPLOY_HOST:-}" ]; then
          host_ip="$DEPLOY_HOST"
        else
          ${infraPkgs.resolveIp}
        fi
        export DEPLOY_HOST="$host_ip"
        export NIX_SSHOPTS="-i $identity"
        ssh_flag="--ssh-opts=-i $identity"
      '';

      stagingDeployPreamble = ''
        export DEPLOY_ENV=staging
        ${deployPreamble}
      '';

      deployFlags = if localSystem == "x86_64-linux" then
        ""
      else
        "--skip-checks --remote-build";

    in {
      deployNixos = pkgs.writeShellApplication {
        name = "deploy-nixos";
        runtimeInputs = deployInputs;
        text = ''
          ${deployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#albion-rest-api.system \
            -- --impure "$@"
        '';
      };

      deployService = pkgs.writeShellApplication {
        name = "deploy-service";
        runtimeInputs = deployInputs;
        text = ''
          ${deployPreamble}
          profile="''${1:?usage: deploy-service <profile>}"
          shift
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} ".#albion-rest-api.$profile" \
            -- --impure "$@"
        '';
      };

      deployAll = pkgs.writeShellApplication {
        name = "deploy-all";
        runtimeInputs = deployInputs;
        text = ''
          ${deployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#albion-rest-api \
            -- --impure "$@"
        '';
      };

      deployStagingNixos = pkgs.writeShellApplication {
        name = "deploy-staging-nixos";
        runtimeInputs = deployInputs;
        text = ''
          ${stagingDeployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#albion-rest-api-staging.system \
            -- --impure "$@"
        '';
      };

      deployStagingService = pkgs.writeShellApplication {
        name = "deploy-staging-service";
        runtimeInputs = deployInputs;
        text = ''
          ${stagingDeployPreamble}
          profile="''${1:-rest-api}"
          shift || true
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} ".#albion-rest-api-staging.$profile" \
            -- --impure "$@"
        '';
      };

      deployStagingAll = pkgs.writeShellApplication {
        name = "deploy-staging-all";
        runtimeInputs = deployInputs;
        text = ''
          ${stagingDeployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#albion-rest-api-staging \
            -- --impure "$@"
        '';
      };

      deployStagingGceNixos = pkgs.writeShellApplication {
        name = "deploy-staging-gce-nixos";
        runtimeInputs = deployInputs;
        text = ''
          ${stagingDeployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#albion-rest-api-staging-gce.system \
            -- --impure "$@"
        '';
      };

      deployStagingGceService = pkgs.writeShellApplication {
        name = "deploy-staging-gce-service";
        runtimeInputs = deployInputs;
        text = ''
          ${stagingDeployPreamble}
          profile="''${1:-rest-api}"
          shift || true
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} ".#albion-rest-api-staging-gce.$profile" \
            -- --impure "$@"
        '';
      };

      deployStagingGceAll = pkgs.writeShellApplication {
        name = "deploy-staging-gce-all";
        runtimeInputs = deployInputs;
        text = ''
          ${stagingDeployPreamble}
          deploy ${deployFlags} ''${ssh_flag:+"$ssh_flag"} .#albion-rest-api-staging-gce \
            -- --impure "$@"
        '';
      };
    };
}
