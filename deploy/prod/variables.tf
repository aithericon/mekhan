# =============================================================================
# Inputs to the prod deploy
# =============================================================================
# Defaults match the prod environment of HetznerCluster (environments/prod/env.hcl).
# Prod is currently the only fully-populated environment in the cluster.
# =============================================================================

# ── Terraform state encryption ──────────────────────────────────────────────

variable "state_encryption_passphrase" {
  description = "Passphrase for PBKDF2-derived AES-GCM key encrypting tfstate"
  type        = string
  sensitive   = true
}

# ── Nomad target ────────────────────────────────────────────────────────────

variable "nomad_address" {
  description = "Prod Nomad HTTP API endpoint. From HetznerCluster/environments/prod/env.hcl: https://10.20.0.10:4646."
  type        = string
}

variable "nomad_region" {
  description = "Nomad region for the prod cluster"
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
  description = "Nomad node class. HetznerCluster has stateless / stateful / ingress / nats — mekhan-service is stateless."
  type        = string
  default     = "stateless"
}

# ── Container image ─────────────────────────────────────────────────────────

variable "image_repository" {
  description = "Container image repo (without tag)"
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
  description = "Public hostname for Traefik. HetznerCluster prod Traefik fronts *.aithericon.eu."
  type        = string
  default     = "mekhan.aithericon.eu"
}

variable "traefik_enabled" {
  description = "If true, mekhan-service registers Traefik routing tags in Consul. Set false for an internal-only deploy."
  type        = bool
  default     = true
}

# ── mekhan-service runtime config ───────────────────────────────────────────

variable "database_url" {
  description = "Postgres URL (against the Patroni cluster in HetznerCluster layer 06b)"
  type        = string
  sensitive   = true
}

variable "nats_url" {
  description = "NATS URL (cluster's shared NATS, layer 04c)"
  type        = string
}

variable "petri_lab_url" {
  description = "URL of the engine (core-engine)"
  type        = string
}

variable "s3_endpoint" {
  description = "S3 endpoint. Hetzner Object Storage: https://fsn1.your-objectstorage.com"
  type        = string
}

variable "s3_bucket" {
  description = "S3 bucket name for artifact storage"
  type        = string
  default     = "mekhan-artifacts"
}

variable "s3_access_key" {
  description = "S3 access key"
  type        = string
  sensitive   = true
}

variable "s3_secret_key" {
  description = "S3 secret key"
  type        = string
  sensitive   = true
}

variable "auth_mode" {
  description = "mekhan-service auth mode. Prod should be 'bff' (real Zitadel)."
  type        = string
  default     = "bff"
  validation {
    condition     = contains(["dev_noop", "bff"], var.auth_mode)
    error_message = "auth_mode must be dev_noop or bff."
  }
}

variable "rust_log" {
  description = "RUST_LOG filter passed to the service"
  type        = string
  default     = "info"
}

# ── Resources ───────────────────────────────────────────────────────────────

variable "service_count" {
  description = "Number of mekhan-service replicas in prod"
  type        = number
  default     = 2
}

variable "cpu_mhz" {
  description = "Nomad CPU reservation in MHz"
  type        = number
  default     = 1000
}

variable "memory_mb" {
  description = "Nomad memory reservation in MB"
  type        = number
  default     = 1024
}

variable "service_port" {
  description = "Port mekhan-service listens on"
  type        = number
  default     = 3100
}
