rec {
  keys = {
    alastair =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJArH3PA+bFIon0JkCVQGs9aWr45lnVjiiTLLO9BPItn";
    github_actions_deploy =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIN8tXytd8vWClKbJ+xSyCFNHlIaR4R4KGOb9IUGaxSlk";
    # Prod droplet host key. Rewritten in place by the `bootstrap-nixos` task
    # (see flake.nix) after nixos-anywhere provisions the host. The staging
    # host key is appended to `roles.infra` at bootstrap time by tooling — it
    # is not tracked here so a manually-provisioned staging droplet does not
    # block prod deploys.
    host =
      "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIVjE3PzTWUFhP+f0gZMQ5/7rFktsUa4xNPd0co4IAks";
  };

  roles = with keys; {
    infra = [ alastair github_actions_deploy ];
    ssh = [ alastair github_actions_deploy host ];
  };
}
