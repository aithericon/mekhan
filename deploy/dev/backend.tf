# =============================================================================
# Terraform state backend — Hetzner Object Storage (S3-compatible)
# =============================================================================
# State key is namespaced by environment so dev + prod can't collide. Bucket /
# endpoint / region are inlined here — they aren't secret and never differ
# per operator, so the partial-backend-config dance was overkill.
#
# AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY come from .envrc (loaded via
# direnv), so `tofu init` alone is enough — no -backend-config flag.
#
# The state file is encrypted client-side (AES-GCM via PBKDF2-derived key)
# using TF_VAR_state_encryption_passphrase from .envrc. Cluster-wide
# convention — every Terragrunt layer in HetznerCluster encrypts the same
# way, so mekhan's state file isn't the odd plaintext object in the bucket.
# =============================================================================

terraform {
  required_version = ">= 1.7.0"

  backend "s3" {
    bucket = "tfstate-aithericon-prod"
    key    = "mekhan/dev/terraform.tfstate"
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
