{
  description = "Albion REST API";

  inputs = {
    rainix.url = "github:rainlanguage/rainix";
    flake-utils.url = "github:numtide/flake-utils";
    ragenix.url = "github:yaxitech/ragenix";
    deploy-rs.url = "github:serokell/deploy-rs";

    crane.url = "github:ipetkov/crane";

    disko.url = "github:nix-community/disko";
    disko.inputs.nixpkgs.follows = "rainix/nixpkgs";

    nixos-anywhere.url = "github:nix-community/nixos-anywhere";
    nixos-anywhere.inputs.nixpkgs.follows = "rainix/nixpkgs";
  };

  outputs =
    {
      self,
      flake-utils,
      rainix,
      ragenix,
      deploy-rs,
      disko,
      nixos-anywhere,
      crane,
      ...
    }:
    let
      mkNixosConfiguration =
        albionEnv:
        rainix.inputs.nixpkgs.lib.nixosSystem {
          system = "x86_64-linux";

          specialArgs = {
            inherit albionEnv;
            docsRoot = self.packages.x86_64-linux.albion-docs;
          };

          modules = [
            disko.nixosModules.disko
            ragenix.nixosModules.default
            ./os.nix
          ] ++ (albionEnv.extraModules or [ ]);
        };
    in
    {
      # Albion prod runs on the DigitalOcean droplet root filesystem — there is
      # no attached DO block volume, so dataVolumeName is null and os.nix skips
      # the /mnt/data mount (data lives on the root fs under dataDir).
      nixosConfigurations.albion-rest-api-prod = mkNixosConfiguration {
        name = "prod";
        virtualHost = "api.albionlabs.org";
        configFile = ./config/prod.toml;
        dataDir = "/mnt/data/albion-rest-api";
        dataVolumeName = null;
      };

      # Staging droplet (138.68.167.234) was provisioned manually with doctl,
      # including the attached DO block volume "albion-rest-api-staging-data".
      nixosConfigurations.albion-rest-api-staging = mkNixosConfiguration {
        name = "staging";
        virtualHost = "138-68-167-234.sslip.io";
        configFile = ./config/staging.toml;
        dataDir = "/mnt/data/albion-rest-api-staging";
        dataVolumeName = "albion-rest-api-staging-data";
        extraModules = [
          (
            { lib, ... }:
            {
              # cloud-init network configuration proved unreliable on this
              # new-generation DO droplet (host booted with no networking) —
              # pin the static addresses DO assigns instead.
              services.cloud-init.network.enable = lib.mkForce false;
              networking = {
                interfaces.eth0.ipv4.addresses = [
                  {
                    address = "138.68.167.234";
                    prefixLength = 20;
                  }
                ];
                defaultGateway = "138.68.160.1";
                nameservers = [
                  "1.1.1.1"
                  "8.8.8.8"
                ];
              };
              # Never block boot waiting on the data volume.
              fileSystems."/mnt/data".options = [
                "nofail"
                "x-systemd.device-timeout=10s"
              ];
            }
          )
        ];
      };

      nixosConfigurations.albion-rest-api = self.nixosConfigurations.albion-rest-api-prod;

      deploy = (import ./deploy.nix { inherit deploy-rs self; }).config;

      checks.x86_64-linux = deploy-rs.lib.x86_64-linux.deployChecks self.deploy;
    }
    // flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import rainix.inputs.nixpkgs {
          inherit system;
          config.allowUnfreePredicate = pkg: builtins.elem (pkgs.lib.getName pkg) [ "terraform" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rainix.rust-toolchain.${system};
      in
      rec {
        packages =
          let
            rainixPkgs = rainix.packages.${system};
            infraPkgs = import ./infra {
              inherit
                pkgs
                ragenix
                rainix
                system
                ;
            };

            deployPkgs = (import ./deploy.nix { inherit deploy-rs self; }).wrappers {
              inherit pkgs infraPkgs;
              localSystem = system;
            };

            albionRust = pkgs.callPackage ./rust.nix {
              inherit craneLib;
              inherit (pkgs) sqlx-cli;
            };
            albion-docs = pkgs.stdenv.mkDerivation {
              pname = "albion-docs";
              version = "0.1.0";
              src = ./docs;
              nativeBuildInputs = [ pkgs.mdbook ];
              buildPhase = "mdbook build";
              installPhase = "cp -r book $out";
            };

          in
          rainixPkgs
          // deployPkgs
          // {
            inherit albion-docs;
            rs-test = rainix.mkTask.${system} {
              name = "rs-test";
              body = ''
                set -euxo pipefail
                cargo test --workspace
              '';
            };
            inherit (infraPkgs)
              tfInit
              tfPlan
              tfApply
              tfImport
              tfDestroy
              tfEditVars
              ;

            albion-rest-api = albionRust.package;
            albion-clippy = albionRust.clippy;

            prepSolArtifacts = rainix.mkTask.${system} {
              name = "prep-sol-artifacts";
              additionalBuildInputs = rainix.sol-build-inputs.${system};
              body = ''
                set -euxo pipefail

                (cd lib/rain.orderbook/ && forge soldeer install && forge build)
              '';
            };

            bootstrap = rainix.mkTask.${system} {
              name = "bootstrap-nixos";
              additionalBuildInputs = infraPkgs.buildInputs ++ [ nixos-anywhere.packages.${system}.default ];
              body = ''
                ${infraPkgs.resolveIp}
                deploy_env="''${DEPLOY_ENV:-prod}"
                case "$deploy_env" in
                  prod) nixos_config="albion-rest-api-prod" ;;
                  staging) nixos_config="albion-rest-api-staging" ;;
                  *)
                    echo "unsupported DEPLOY_ENV '$deploy_env' (expected prod or staging)" >&2
                    exit 1
                    ;;
                esac
                ssh_opts="-o StrictHostKeyChecking=no -o ConnectTimeout=5 -i $identity"

                nixos-anywhere --flake ".#$nixos_config" \
                  --option pure-eval false \
                  --ssh-option "IdentityFile=$identity" \
                  --target-host "root@$host_ip" "$@"

                echo "Waiting for host to come back up..."
                retries=0
                until ssh $ssh_opts "root@$host_ip" true 2>/dev/null; do
                  retries=$((retries + 1))
                  if [ "$retries" -ge 60 ]; then
                    echo "Host did not come back up after 5 minutes" >&2
                    exit 1
                  fi
                  sleep 5
                done

                new_key=$(
                  ssh $ssh_opts "root@$host_ip" \
                    cat /etc/ssh/ssh_host_ed25519_key.pub \
                    | awk '{print $1 " " $2}'
                )

                valid_key='^ssh-ed25519 [A-Za-z0-9+/=]+$'
                if [ -z "$new_key" ] || ! echo "$new_key" | grep -qE "$valid_key"; then
                  echo "ERROR: SSH host key is empty or malformed: '$new_key'" >&2
                  exit 1
                fi

                if [ "$deploy_env" = "prod" ]; then
                  ${pkgs.gnused}/bin/sed -i \
                    '/host =/{n;s|"ssh-ed25519 [A-Za-z0-9+/=]*"|"'"$new_key"'"|;}' \
                    keys.nix

                  echo "Updated host key in keys.nix"
                else
                  echo "Staging SSH host key:"
                  echo "$new_key"
                  echo
                  echo "Optional GitHub secret STAGING_SSH_HOST_KEY value:"
                  echo "$new_key"
                fi
              '';
            };

            tfRekey = rainix.mkTask.${system} {
              name = "tf-rekey";
              additionalBuildInputs = infraPkgs.buildInputs;
              body = infraPkgs.tfRekey;
            };

            tfStagingProvision = rainix.mkTask.${system} {
              name = "tf-staging-provision";
              additionalBuildInputs = infraPkgs.buildInputs;
              body = infraPkgs.tfPreviewProvision;
            };

            resolveIp = pkgs.writeShellApplication {
              name = "resolve-ip";
              runtimeInputs = infraPkgs.buildInputs;
              text = ''
                ${infraPkgs.resolveIp}
                echo "$host_ip"
              '';
            };

            resolveStagingIp = pkgs.writeShellApplication {
              name = "resolve-staging-ip";
              runtimeInputs = infraPkgs.buildInputs;
              text = ''
                export DEPLOY_ENV=staging
                ${infraPkgs.resolveIp}
                echo "$host_ip"
              '';
            };

            remote = pkgs.writeShellApplication {
              name = "remote";
              runtimeInputs = infraPkgs.buildInputs ++ [ pkgs.openssh ];
              text = ''
                ${infraPkgs.resolveIp}
                exec ssh -i "$identity" "root@$host_ip" "$@"
              '';
            };

            remoteStaging = pkgs.writeShellApplication {
              name = "remote-staging";
              runtimeInputs = infraPkgs.buildInputs ++ [ pkgs.openssh ];
              text = ''
                export DEPLOY_ENV=staging
                ${infraPkgs.resolveIp}
                exec ssh -i "$identity" "root@$host_ip" "$@"
              '';
            };

            stagingCreateApiKey = pkgs.writeShellApplication {
              name = "staging-create-api-key";
              runtimeInputs = infraPkgs.buildInputs ++ [ pkgs.openssh ];
              text = ''
                usage() {
                  echo "usage: staging-create-api-key <label> <owner> [--admin]" >&2
                }

                label="''${1:-}"
                owner="''${2:-}"
                admin="''${3:-}"

                if [ -z "$label" ] || [ -z "$owner" ]; then
                  usage
                  exit 1
                fi

                if [ -n "$admin" ] && [ "$admin" != "--admin" ]; then
                  usage
                  exit 1
                fi

                export DEPLOY_ENV=staging
                ${infraPkgs.resolveIp}

                remote_cmd=(
                  /nix/var/nix/profiles/per-service/rest-api/bin/albion_rest_api
                  keys
                  --config
                  /etc/albion-rest-api/config.toml
                  create
                  --label
                  "$label"
                  --owner
                  "$owner"
                )

                if [ "$admin" = "--admin" ]; then
                  remote_cmd+=(--admin)
                fi

                printf -v remote_cmd_escaped '%q ' "''${remote_cmd[@]}"
                exec ssh -i "$identity" "root@$host_ip" "''${remote_cmd_escaped% }"
              '';
            };

          };

        formatter = pkgs.nixfmt-classic;

        devShells.default = pkgs.mkShell {
          inherit (rainix.devShells.${system}.default) nativeBuildInputs;
          shellHook = rainix.devShells.${system}.default.shellHook + ''
            export COMMIT_SHA="$(git rev-parse HEAD 2>/dev/null || echo "dev")"
          '';
          buildInputs =
            with pkgs;
            [
              cargo-nextest
              sqlx-cli
              terraform
              mdbook
              ragenix.packages.${system}.default
              packages.rs-test
              packages.prepSolArtifacts
              packages.resolveIp
              packages.remote
              packages.remoteStaging
              packages.stagingCreateApiKey
              packages.resolveStagingIp
              packages.deployNixos
              packages.deployService
              packages.deployAll
              packages.deployStagingNixos
              packages.deployStagingService
              packages.deployStagingAll
            ]
            ++ rainix.devShells.${system}.default.buildInputs;
        };
      }
    );

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };
}
