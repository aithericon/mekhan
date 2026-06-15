# =============================================================================
# Terraform state backend — Hetzner Object Storage (S3-compatible)
# =============================================================================
# Mirrors deploy/dev/backend.tf exactly — same bucket/endpoint/region — except
# the state key, which is namespaced `mekhan/prod/...` so dev and prod can't
# collide. This is the ONLY .tf file that legitimately differs from deploy/dev;
# everything else is byte-identical (diff deploy/dev deploy/prod to confirm).
#
# AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY come from .envrc / the CI step env
# (loaded via direnv locally), so `tofu init` alone is enough — no
# -backend-config flag. The state file is AES-GCM encrypted client-side using
# TF_VAR_state_encryption_passphrase, the cluster-wide convention.
# =============================================================================

terraform {
  required_version = ">= 1.7.0"

  backend "s3" {
    bucket = "tfstate-aithericon-prod"
    key    = "mekhan/prod/terraform.tfstate"
    region = "fsn1"
    endpoints = {
      s3 = "https://fsn1.your-objectstorage.com"
    }

    # Hetzner Object Storage is S3-API-compatible but not AWS; skip every
    # AWS-specific validation step.
    use_path_style              = true
    skip_credentials_validation = true
    skip_metadata_api_check     = true
    skip_region_validation      = true
    skip_requesting_account_id  = true
  }

  encryption {
    key_provider "pbkdf2" "state" {
      passphrase = var.state_encryption_passphrase
    }
    method "aes_gcm" "state" {
      keys = key_provider.pbkdf2.state
    }
    state {
      method   = method.aes_gcm.state
      enforced = true
    }
  }
}
