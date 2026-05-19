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
    # Hetzner Object Storage is S3-compatible — we use the AWS provider with
    # the endpoint overridden. Same pattern the tfstate backend already uses.
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
    # Talks directly to the cluster's Zitadel at id.aithericon.eu via its
    # admin API. Same shape mekhan already uses for cyrilgdn/postgresql —
    # the service repo owns its OIDC application registration the same way
    # it owns its Postgres role.
    zitadel = {
      source  = "zitadel/zitadel"
      version = "~> 1.2"
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

# Hetzner Object Storage — S3-compatible. Region is the Hetzner location code
# (fsn1 = Falkenstein). The path-style + checksum settings are required: the
# Hetzner endpoint doesn't support virtual-hosted bucket URLs and rejects the
# v4 trailing-checksum probes that AWS SDKs added by default.
provider "aws" {
  region                      = "fsn1"
  access_key                  = var.s3_access_key
  secret_key                  = var.s3_secret_key
  skip_credentials_validation = true
  skip_metadata_api_check     = true
  skip_region_validation      = true
  skip_requesting_account_id  = true

  endpoints {
    s3 = var.s3_endpoint
  }

  s3_use_path_style = true
}

# Zitadel — the cluster runs one instance at id.aithericon.eu shared across
# all services. `var.zitadel_pat` is a Personal Access Token issued to the
# IaC service user (stashed in Vault at secret/zitadel/iac-pat → field
# `token`). The CI step exports it from Vault; locally, paste it into
# .envrc. Insecure=false (TLS verify on) — Zitadel's cert is from
# Let's Encrypt via Traefik.
provider "zitadel" {
  domain   = "id.aithericon.eu"
  insecure = false
  port     = "443"
  token    = var.zitadel_pat
}
