# =============================================================================
# Inputs to the dev deploy
# =============================================================================
# Defaults match the dev environment of HetznerCluster (environments/dev/env.hcl).
# Non-secret values live in dev.auto.tfvars (gitignored — copy from .example).
# Secrets are passed via TF_VAR_<name> env vars sourced from Woodpecker secrets.
# =============================================================================

# ── Terraform state encryption ──────────────────────────────────────────────

variable "state_encryption_passphrase" {
  description = "Passphrase for PBKDF2-derived AES-GCM key encrypting tfstate. Must match the passphrase used by other HetznerCluster layers if state is shared."
  type        = string
  sensitive   = true
}

# ── Nomad target ────────────────────────────────────────────────────────────

variable "nomad_address" {
  description = "Dev Nomad HTTP API endpoint. From HetznerCluster/environments/dev/env.hcl: https://10.30.0.10:4646 (Tailscale/WireGuard internal IP)."
  type        = string
}

variable "nomad_region" {
  description = "Nomad region for the dev cluster"
  type        = string
  default     = "global"
}

variable "nomad_token" {
  description = "Nomad ACL token with submit-job + read-job on the mekhan namespace"
  type        = string
  sensitive   = true
}

variable "nomad_namespace" {
  description = "Nomad namespace the mekhan-service job runs in"
  type        = string
  default     = "default"
}

variable "nomad_datacenters" {
  description = "Datacenters the job is eligible for"
  type        = list(string)
  default     = ["dc1"]
}

variable "node_class" {
  description = "Nomad node class to schedule on. HetznerCluster uses: ingress, stateless, stateful, nats. mekhan-service is stateless."
  type        = string
  default     = "stateless"
}

# ── Container image ─────────────────────────────────────────────────────────

variable "image_repository" {
  description = "Container image repo (without tag). HetznerCluster default registry is forge.aithericon.eu."
  type        = string
}

variable "image_tag" {
  description = "Tag of the image to deploy. CI sets this to CI_COMMIT_SHA."
  type        = string
}

variable "registry_user" {
  description = "Username for the private registry the Nomad clients pull from"
  type        = string
  sensitive   = true
}

variable "registry_password" {
  description = "Password for the private registry"
  type        = string
  sensitive   = true
}

# ── External exposure (Traefik) ─────────────────────────────────────────────

variable "hostname" {
  description = "Public hostname for Traefik to route to mekhan-service. HetznerCluster Traefik discovers via Consul + ACMEs Let's Encrypt with Cloudflare DNS validation."
  type        = string
  default     = "mekhan.dev.aithericon.eu"
}

variable "traefik_enabled" {
  description = "If true, mekhan-service registers Traefik routing tags in Consul and gets a public HTTPS endpoint at var.hostname. Set false for an internal-only deploy."
  type        = bool
  default     = true
}

variable "postgres_admin_host" {
  description = "Patroni primary as resolvable from the OPERATOR's machine. Uses the Consul alt-domain so it doesn't clash with any local .service.consul. From HetznerCluster env.hcl: postgres-primary.service.consul.aithericon"
  type        = string
  default     = "postgres-primary.service.consul.aithericon"
}

variable "postgres_runtime_host" {
  description = "Patroni primary as resolvable from inside a Nomad alloc. Plain .service.consul — Nomad clients resolve it via the cluster's Consul DNS. Baked into the MEKHAN__DATABASE_URL the service sees."
  type        = string
  default     = "postgres-primary.service.consul"
}

variable "postgres_admin_password" {
  description = "Patroni superuser password. Fetched from Vault once and pasted into .envrc — see secret/postgres/patroni field superuser_password."
  type        = string
  sensitive   = true
}

variable "nats_url" {
  description = "NATS URL the dev mekhan-service connects to (the cluster's shared NATS, layer 04c)"
  type        = string
}

variable "vault_addr" {
  description = "Vault server address as resolvable from inside a Nomad alloc. Nomad's `vault {}` stanza already injects VAULT_TOKEN via workload-identity exchange; VAULT_ADDR is rendered here so mekhan-service's VaultResourceStore, the engine's secret-wrapping path, and the executor's unwrap call all reach the same server. HetznerCluster Vault: http://10.20.0.20:8200."
  type        = string
  default     = "http://10.20.0.20:8200"
}

variable "petri_lab_url" {
  description = "URL of the engine (core-engine) the dev mekhan-service talks to"
  type        = string
}

variable "s3_endpoint" {
  description = "S3 endpoint for artifact storage. Hetzner Object Storage: https://fsn1.your-objectstorage.com"
  type        = string
}

variable "s3_bucket" {
  description = "S3 bucket name for artifact storage"
  type        = string
  default     = "mekhan-artifacts-dev"
}

variable "s3_access_key" {
  description = "S3 access key for the artifact bucket"
  type        = string
  sensitive   = true
}

variable "s3_secret_key" {
  description = "S3 secret key for the artifact bucket"
  type        = string
  sensitive   = true
}

variable "auth_mode" {
  description = "mekhan-service auth mode: dev_noop or bff"
  type        = string
  default     = "dev_noop"
  validation {
    condition     = contains(["dev_noop", "bff"], var.auth_mode)
    error_message = "auth_mode must be dev_noop or bff."
  }
}

# ── Zitadel ─────────────────────────────────────────────────────────────────

variable "zitadel_jwt_file" {
  description = "Path to a JWT profile JSON file for the Zitadel IaC service user. The Zitadel TF provider opens this path; it is NOT the JWT contents. CI fetches the key from Vault (secret/zitadel/iac-jwt field: key) and writes it to /tmp before invoking tofu. Locally, paste your own path into .envrc."
  type        = string
  sensitive   = true
}

variable "zitadel_issuer_url" {
  description = "Public Zitadel issuer URL — baked into MEKHAN__AUTH__ISSUER_URL on the service."
  type        = string
  default     = "https://id.aithericon.eu"
}

variable "zitadel_org_id" {
  description = <<-EOT
    DEPRECATED / UNUSED. Mekhan now creates its own dedicated org
    ("Mekhan Testers") in zitadel.tf and pins every resource to it via
    zitadel_org.mekhan_testers.id. This variable is kept only so the CI export
    (TF_VAR_zitadel_org_id from Vault) and any local tfvars line don't error.
    Safe to remove once CI stops exporting it.
  EOT
  type        = string
  default     = ""
}

variable "tester_users" {
  description = <<-EOT
    TF-managed accounts in the dedicated "Mekhan Testers" Zitadel org. In
    practice this holds only the bootstrap admin — real testers are onboarded
    in-app via Mekhan's invite feature, not here. Each entry becomes a
    zitadel_human_user with a random initial password (published to Vault at
    secret/services/mekhan/dev/testers/<key>) and a grant of `role` on the
    Mekhan project.

    Membership in this isolated org is what scopes the account to Mekhan only —
    NOT a member of the cluster's default org, so no access to Vault / Nomad /
    Grafana / Matrix.

    Map key is a stable slug; the `role` must be one of the project roles
    defined in zitadel.tf (mekhan_user | mekhan_admin).
  EOT
  type = map(object({
    username   = string
    first_name = string
    last_name  = string
    email      = string
    role       = optional(string, "mekhan_user")
  }))
  default = {}

  validation {
    condition     = alltrue([for u in values(var.tester_users) : contains(["mekhan_user", "mekhan_admin"], u.role)])
    error_message = "tester_users[*].role must be mekhan_user or mekhan_admin."
  }
}

variable "workspace_owner_user_key" {
  description = <<-EOT
    Key in `tester_users` to seed as OWNER of the "Testers" Mekhan workspace
    (one-time bootstrap). This is the account that logs in and sends in-app
    invites. Empty string ⇒ no owner is computed and the bootstrap-SQL output
    is null. Must reference an existing key in tester_users.
  EOT
  type        = string
  default     = ""

  validation {
    condition     = var.workspace_owner_user_key == "" || contains(keys(var.tester_users), var.workspace_owner_user_key)
    error_message = "workspace_owner_user_key must be \"\" or a key present in tester_users."
  }
}

variable "rust_log" {
  description = "RUST_LOG filter passed to the service"
  type        = string
  default     = "info,mekhan_service=debug"
}

# ── Invite email (Phase 4) ────────────────────────────────────────────────────
# Drives the in-app invite feature's accept-link delivery. With mode=smtp the
# service sends the link via the relay; with mode=log it only writes it to the
# service log (offline-friendly default). public_base_url is derived from
# var.hostname in main.tf.

variable "email_mode" {
  description = "Invite-email delivery mode: 'smtp' (send via relay) or 'log' (write accept link to service log only)."
  type        = string
  default     = "log"

  validation {
    condition     = contains(["smtp", "log"], var.email_mode)
    error_message = "email_mode must be 'smtp' or 'log'."
  }
}

variable "email_from_address" {
  description = "From address on invite emails (must be an address the SMTP relay is allowed to send as, e.g. id@aithericon.eu)."
  type        = string
  default     = "id@aithericon.eu"
}

variable "email_smtp_host" {
  description = "SMTP relay host (only used when email_mode=smtp)."
  type        = string
  default     = ""
}

variable "email_smtp_port" {
  description = "SMTP relay port (e.g. 587 for STARTTLS)."
  type        = number
  default     = 587
}

variable "email_smtp_username" {
  description = "SMTP relay username. Secret — inject via TF_VAR_email_smtp_username from Vault in CI; do NOT commit."
  type        = string
  default     = ""
  sensitive   = true
}

variable "email_smtp_password" {
  description = "SMTP relay password. Secret — inject via TF_VAR_email_smtp_password from Vault in CI; do NOT commit."
  type        = string
  default     = ""
  sensitive   = true
}

# ── Resources ───────────────────────────────────────────────────────────────

variable "cpu_mhz" {
  description = "Nomad CPU reservation in MHz"
  type        = number
  default     = 500
}

variable "memory_mb" {
  description = "Nomad memory reservation in MB"
  type        = number
  default     = 512
}

# ── Executor (sibling task in the same group) ───────────────────────────────

variable "executor_image_repository" {
  description = "Registry path for the executor image, tagged with image_tag (= mekhan-service's SHA — both ship from the same monorepo commit)."
  type        = string
  default     = "forge.aithericon.eu/milanender/aithericon-executor"
}

variable "executor_cpu_mhz" {
  description = "Executor CPU reservation. Heavier than the service: kreuzberg + tesseract + python venvs are CPU-hungry."
  type        = number
  default     = 1000
}

variable "executor_memory_mb" {
  description = "Executor memory reservation. HDF5 / NetCDF parsers + Python venvs need headroom. Bumped 1024 → 2048 (2026-06-10) after the meeting-recording workflow's transcribe step (whisper-small + faster-whisper BatchedInferencePipeline) got SIGKILLed during inference at the 1 GB ceiling — model weights + pip cache + working tensors exceed 1 GB during cold-start runs."
  type        = number
  default     = 2048
}

variable "executor_concurrency" {
  description = "EXECUTOR_CONCURRENCY env var — number of parallel work items a single executor alloc processes."
  type        = number
  default     = 4
}

variable "executor_count" {
  description = "How many executor allocs to run. The executor is its own Nomad job (split out of mekhan-service so it scales independently); bump this to fan work-pickup out across more nodes."
  type        = number
  default     = 1
}

# ── Engine (separate Nomad job; reached via engine.service.consul:3030) ─────

variable "engine_image_repository" {
  description = "Registry path for the engine image, tagged with image_tag (same SHA as service + executor — all three ship from the same monorepo commit)."
  type        = string
  default     = "forge.aithericon.eu/milanender/aithericon-engine"
}

variable "engine_service_port" {
  description = "Port the engine binary listens on. Must match PORT env var; mekhan-service hard-codes :3030 in default_petri_lab_url."
  type        = number
  default     = 3030
}

variable "engine_cpu_mhz" {
  description = "Engine CPU reservation. Lighter than executor — no kreuzberg/tesseract."
  type        = number
  default     = 500
}

variable "engine_memory_mb" {
  description = "Engine memory reservation."
  type        = number
  default     = 512
}

variable "service_port" {
  description = "Port mekhan-service listens on (matches MEKHAN__PORT in the image)"
  type        = number
  default     = 3100
}
