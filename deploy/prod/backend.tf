# =============================================================================
# Terraform state backend — Hetzner Object Storage (S3-compatible)
# =============================================================================
# Production state lives in a separate bucket from dev so a stray dev apply
# can't even reach prod state (different access keys can be issued for each).
#
# State files are AES-GCM encrypted client-side via the
# TF_VAR_state_encryption_passphrase env var — same convention as every other
# layer in the HetznerCluster repo.
# =============================================================================

terraform {
  required_version = ">= 1.7.0"

  backend "s3" {
    key = "mekhan/prod/terraform.tfstate"

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
