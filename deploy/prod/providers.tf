# =============================================================================
# Providers — Nomad target cluster for production
# =============================================================================
# Production cluster credentials. The CI pipeline supplies var.nomad_token via
# TF_VAR_nomad_token sourced from the `prod_nomad_token` Woodpecker secret —
# distinct from the dev token so a dev role can't apply against prod.
# =============================================================================

terraform {
  required_providers {
    nomad = {
      source  = "hashicorp/nomad"
      version = "~> 2.2"
    }
  }
}

provider "nomad" {
  address   = var.nomad_address
  region    = var.nomad_region
  secret_id = var.nomad_token
}
