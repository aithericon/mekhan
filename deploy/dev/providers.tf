# =============================================================================
# Providers — Nomad + Postgres (cluster Patroni)
# =============================================================================
# The postgresql provider connects directly to the Patroni primary as the
# cluster superuser so it can `CREATE ROLE` + `CREATE DATABASE` for mekhan.
# Superuser password is sourced from .envrc (TF_VAR_postgres_admin_password)
# — operator fetches it once from Vault and pastes it in:
#
#     vault kv get -field=superuser_password secret/postgres/patroni
#
# (Vault provider is intentionally NOT used here. Pulling the password through
# .envrc keeps mekhan's blast radius scoped to ONE cluster secret instead of
# the broader Vault token surface.)
# =============================================================================

terraform {
  required_providers {
    nomad = {
      source  = "hashicorp/nomad"
      version = "~> 2.2"
    }
    postgresql = {
      source  = "cyrilgdn/postgresql"
      version = "~> 1.21"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.6"
    }
  }
}

provider "nomad" {
  address   = var.nomad_address
  region    = var.nomad_region
  secret_id = var.nomad_token
}

# Connects to the cluster's Patroni primary. Host uses the Consul alt-domain
# (consul.aithericon) so DNS resolution works from the operator's machine
# without colliding with any other cluster's .service.consul namespace —
# same trick HetznerCluster's root.hcl uses for cluster-side TF applies.
provider "postgresql" {
  host            = var.postgres_admin_host
  port            = 5432
  database        = "postgres"
  username        = "postgres"
  password        = var.postgres_admin_password
  sslmode         = "disable"
  connect_timeout = 15
}
