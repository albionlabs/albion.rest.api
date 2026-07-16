# Digital Ocean Deployment Guide

This project deploys a NixOS droplet on DigitalOcean via Terraform, then
installs the service using `nixos-anywhere` and `deploy-rs`. All tooling runs
inside the Nix dev shell.

---

## Albion environments

| Environment | nixosConfiguration        | virtualHost               | Data location                                                          |
| ----------- | ------------------------- | ------------------------- | ---------------------------------------------------------------------- |
| **prod**    | `albion-rest-api-prod`    | `api.albionlabs.org`      | root filesystem, `/mnt/data/albion-rest-api` (no DO block volume)      |
| **staging** | `albion-rest-api-staging` | `138-68-167-234.sslip.io` | DO block volume `albion-rest-api-staging-data`, mounted at `/mnt/data` |

Notes:

- **Prod has no attached DigitalOcean block volume.** `dataVolumeName` is `null`
  in `flake.nix`, and `os.nix` only declares the `/mnt/data` mount when
  `dataVolumeName != null`. Prod data lives on the droplet root filesystem.
- **The staging droplet (`138.68.167.234`) and its block volume
  (`albion-rest-api-staging-data`) were provisioned manually with `doctl`**, not
  through this repo's Terraform. Staging has no Terraform state entry, so
  staging deploys resolve the host via the `DEPLOY_HOST` environment variable
  (the `deploy.nix` preamble short-circuits Terraform IP resolution when
  `DEPLOY_HOST` is set). Example:
  `DEPLOY_HOST=138.68.167.234 nix run .#deployStagingAll`.
- Secret RPC keys (`DRPC_API_KEY`, `ALCHEMY_API_KEY`, referenced by the
  `[additional_rpcs]` config table via `${VAR}`) are supplied to the service via
  `EnvironmentFile=/etc/albion/<name>.env` (e.g. `/etc/albion/prod.env`). The
  leading `-` makes a missing file non-fatal on first boot.

---

## Prerequisites

### 1. Nix with Flakes

Install Nix and enable flakes:

```bash
# Install Nix (if not already installed)
sh <(curl -L https://nixos.org/nix/install) --daemon

# Enable flakes in ~/.config/nix/nix.conf
echo "experimental-features = nix-command flakes" >> ~/.config/nix/nix.conf
```

### 2. SSH key pair

The deployment uses an ed25519 key. Generate one if needed:

```bash
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519
```

### 3. DigitalOcean account setup

- Create a **Personal Access Token** (read+write) at: DigitalOcean → API →
  Tokens
- Upload your SSH public key to DigitalOcean under: Settings → Security → SSH
  Keys
  - Note the **name** you give it — you'll need it below (default expected:
    `st0x-op`)

### 4. Enter the Nix dev shell

All commands below must be run inside this shell:

```bash
nix develop
```

---

## Step 1 — Add your SSH key to `keys.nix`

Open `keys.nix` and add your SSH public key to the `keys` map and the relevant
roles:

```nix
rec {
  keys = {
    your-name = "ssh-ed25519 AAAA...your-pubkey...";
    # ... existing keys
  };

  roles = {
    infra = [ keys.your-name keys.ci ];
    ssh   = [ keys.your-name keys.ci keys.arda ];
  };
}
```

This controls who can decrypt secrets (`infra` role) and SSH into the server
(`ssh` role).

---

## Step 2 — Configure Terraform variables

The Terraform variables are stored encrypted. Use the helper to create/edit
them:

```bash
nix develop -c tf-edit-vars
```

This opens your `$EDITOR` (defaults to `vi`) with the decrypted vars file. Fill
in the required value:

```hcl
do_token = "your-digitalocean-api-token"
```

Optional overrides (defaults shown):

```hcl
ssh_key_name   = "st0x-op"   # Name of your SSH key in DigitalOcean
region         = "nyc3"       # DigitalOcean region slug
droplet_size   = "s-2vcpu-4gb"
volume_size_gb = 5
```

Save and exit — the file is automatically re-encrypted with `rage` using the
keys in `keys.nix`. **Never commit the plaintext `infra/terraform.tfvars`.**

---

## Step 3 — Provision infrastructure with Terraform

```bash
# Initialize Terraform providers
nix develop -c tf-init

# Preview what will be created
nix develop -c tf-plan

# Apply — creates droplet, volume, and reserved IP
nix develop -c tf-apply
```

This provisions:

- Ubuntu 24.04 droplet (`st0x-rest-api-nixos`) in the chosen region
- 5 GB block storage volume (`st0x-rest-api-data`) mounted at `/mnt/data`
- A reserved IP attached to the droplet

The Terraform state is encrypted with `rage` and committed as
`infra/terraform.tfstate.age`.

---

## Step 4 — Bootstrap NixOS onto the droplet

The droplet boots Ubuntu. This step installs NixOS over it using
`nixos-anywhere`:

```bash
nix develop -c bootstrap-nixos
```

This command will:

1. Resolve the droplet IP from the Terraform state
2. Run `nixos-anywhere` to partition the disk (via `disko.nix`) and install
   NixOS
3. Wait for the host to reboot
4. Read the new SSH host key from the server
5. **Automatically update `keys.nix`** with the real host key

After this step, commit the updated `keys.nix`:

```bash
git add keys.nix
git commit -m "chore: update host SSH key after bootstrap"
```

---

## Step 5 — Re-encrypt secrets with the host key

Now that the host key is in `keys.nix`, re-encrypt all secrets so the server can
decrypt them at runtime:

```bash
nix develop -c tf-rekey
```

Commit the re-encrypted secret files:

```bash
git add infra/terraform.tfvars.age infra/terraform.tfstate.age
git commit -m "chore: rekey secrets with new host key"
```

---

## Step 6 — Deploy the full stack

Deploy both the NixOS system config and the REST API service in one command:

```bash
nix develop -c deploy-all
```

Or deploy them separately:

```bash
# Deploy only the OS/system configuration
nix develop -c deploy-nixos

# Deploy only the REST API service binary
nix develop -c deploy-service rest-api
```

The deploy-rs workflow:

- Builds the Nix derivation locally (or cross-builds for non-Linux hosts)
- Copies the closure to the remote via SSH
- Activates the system profile / restarts the service

---

## Step 7 — DNS and TLS

1. Point your domain (`api.st0x.io` or your fork's domain) to the **reserved
   IP** output by Terraform:
   ```bash
   nix develop -c resolve-ip   # prints the reserved IP
   ```
2. Nginx is pre-configured to terminate TLS via Let's Encrypt (ACME). TLS
   certificates are issued automatically on first HTTP request to port 80.

Check the domain in `os.nix`:

```nix
virtualHosts."api.st0x.io" = { ... };
```

Update it to your domain before deploying if this is a fork.

Also update the ACME contact email:

```nix
security.acme.defaults.email = "ops@your-domain.io";
```

---

## Step 8 — Create an API key

SSH into the server and create the first API key:

```bash
nix develop -c remote   # opens an SSH session as root

# On the server:
/nix/var/nix/profiles/per-service/rest-api/bin/st0x_rest_api \
  keys create --config /nix/var/nix/profiles/per-service/rest-api/../../../... \
  --name "admin"
```

Or more practically, check the systemd service for the exact binary path and
config:

```bash
systemctl cat rest-api
```

---

## Post-deployment — Ongoing operations

### Redeploy after code changes

```bash
nix develop -c deploy-service rest-api
```

### Deploy a branch to preview

Preview is a separate reusable machine and volume. It is intended for testing
branch builds under production-like conditions without touching production data
or services.

Provision preview by setting `preview_enabled = true` in the encrypted Terraform
vars, then plan/apply:

```bash
nix develop -c tf-edit-vars
nix develop -c tf-plan
nix develop -c tf-apply
```

Bootstrap preview after the host exists:

```bash
DEPLOY_ENV=preview nix develop -c bootstrap
```

Point `api.staging.st0x.io` at the preview reserved IP:

```bash
nix develop -c resolve-preview-ip
```

Deploy the checked-out branch to preview:

```bash
nix develop -c deploy-preview-all
```

For service-only branch deploys after the preview system is already configured:

```bash
nix develop -c deploy-preview-service
```

Preview service deploys intentionally reset local preview state before restart:

- stop `rest-api`
- remove `/mnt/data/st0x-rest-api-preview/st0x.db`
- remove `/mnt/data/st0x-rest-api-preview/raindex.db`
- preserve `/mnt/data/st0x-rest-api-preview/private-registry.data`
- restart `rest-api` from the branch build

After deploy, wait for `/health/detailed` to report a ready raindex state before
running performance checks:

```bash
export API_URL=https://api.staging.st0x.io
read -r -p "API_KEY: " API_KEY && export API_KEY
read -r -s -p "API_SECRET: " API_SECRET && export API_SECRET
printf "\n"
./scripts/smoke.sh
```

Because the preview app DB is reset on preview service deploys, create a fresh
preview API key after deploy when you need to run authenticated checks:

```bash
nix develop -c preview-create-api-key "preview-benchmark" "arda@st0x.io"
```

The command runs only against the preview host and prints the new key ID and
secret in the terminal. Add `--admin` if the preview key needs admin access:

```bash
nix develop -c preview-create-api-key "preview-admin" "arda@st0x.io" --admin
```

### Deploy preview from GitHub

The `Deploy Preview` GitHub Actions workflow exposes a manual
`workflow_dispatch` button. It accepts:

- `ref`: branch, tag, or SHA to deploy
- `deploy_scope`: `service` for the API binary/profile only, or `all` for the
  preview NixOS system plus service

The workflow:

- checks out the selected ref
- resolves the preview reserved IP from Terraform state
- deploys to the preview deploy-rs node
- resets preview DB state as part of the preview service activation

Required repository secrets:

- `SSH_KEY`: private key allowed to SSH to the preview host
- `PREVIEW_SSH_HOST_KEY`: optional but recommended SSH host public key for the
  preview machine. If it is absent, the workflow falls back to `ssh-keyscan` for
  the preview host.

### SSH into the server

```bash
nix develop -c remote
```

### SSH into preview

```bash
nix develop -c remote-preview
```

### View service logs

```bash
nix develop -c remote
# on server:
journalctl -u rest-api -f
# or log files:
ls /mnt/data/st0x-rest-api/logs/
```

### Check service status

```bash
nix develop -c remote
# on server:
systemctl status rest-api
```

### Tear down infrastructure

```bash
nix develop -c tf-destroy
```

---

## Architecture summary

```
Your machine
  └─ nix develop shell
       ├─ Terraform (infra/)   → DigitalOcean API → Droplet + Volume + Reserved IP
       ├─ nixos-anywhere        → SSH into droplet → Install NixOS
       └─ deploy-rs             → SSH into server  → Deploy system + service

Server (NixOS on DigitalOcean)
  ├─ Nginx (443)  → reverse proxy → Rocket API (127.0.0.1:8000)
  ├─ SQLite DB    → /mnt/data/st0x-rest-api/st0x.db
  ├─ Logs         → /mnt/data/st0x-rest-api/logs/
  └─ /mnt/data    → DigitalOcean block volume (persists across reboots)
```

---

## Environment variables

Set `RUST_LOG` to control log verbosity. The deployed systemd service sets this
in `os.nix`:

```
RUST_LOG=st0x_rest_api=info,raindex_common=info,raindex_quote=info,rocket=warn,warn
```

To change it on the server, update `os.nix` and redeploy with `deploy-nixos`.
