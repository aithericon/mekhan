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

# ── mekhan-service runtime config ───────────────────────────────────────────
# These point at infra deployed by sibling HetznerCluster layers:
#   Postgres  — layer 06b (service: postgres.service.consul:5432)
#   NATS      — layer 04c (service: nats.service.consul:4222)
#   rustfs/S3 — Hetzner Object Storage (fsn1.your-objectstorage.com)
#   Zitadel   — layer 06d/e (id.dev.aithericon.eu)

# ── Postgres admin (for `CREATE ROLE/DATABASE` at apply time) ────────────────

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

variable "zitadel_pat" {
  description = "Personal Access Token for the Zitadel IaC service user. Used by the zitadel TF provider to manage mekhan's project + OIDC application. Sourced from Vault at secret/zitadel/iac-pat (field: token)."
  type        = string
  sensitive   = true
}

variable "zitadel_issuer_url" {
  description = "Public Zitadel issuer URL — baked into MEKHAN__AUTH__ISSUER_URL on the service."
  type        = string
  default     = "https://id.aithericon.eu"
}

variable "rust_log" {
  description = "RUST_LOG filter passed to the service"
  type        = string
  default     = "info,mekhan_service=debug"
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

variable "service_port" {
  description = "Port mekhan-service listens on (matches MEKHAN__PORT in the image)"
  type        = number
  default     = 3100
}
