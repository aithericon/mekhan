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

    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }

    zitadel = {
      source  = "zitadel/zitadel"
      version = "~> 1.2"
    }
    vault = {
      source  = "hashicorp/vault"
      version = "~> 4.4"
    }
  }
}

provider "nomad" {
  address   = var.nomad_address
  region    = var.nomad_region
  secret_id = var.nomad_token
}


provider "postgresql" {
  host            = var.postgres_admin_host
  port            = 5432
  database        = "postgres"
  username        = "postgres"
  password        = var.postgres_admin_password
  sslmode         = "disable"
  connect_timeout = 15
}

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

# path to your own JWT JSON into .envrc.
provider "zitadel" {
  domain           = "id.aithericon.eu"
  insecure         = false
  port             = "443"
  jwt_profile_file = var.zitadel_jwt_file
}


provider "vault" {}
