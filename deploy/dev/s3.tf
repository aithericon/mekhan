# =============================================================================
# mekhan-service artifact bucket — Hetzner Object Storage
# =============================================================================
# Provisions the bucket mekhan-service uploads template assets / blobs into.
# Without this, the running service authenticates fine but every PUT returns
# "service error" because the bucket simply doesn't exist on the endpoint.
#
# Naming is driven by var.s3_bucket (default "mekhan-artifacts-dev") so this
# resource is reusable across envs — when prod splits out, just pass a
# different value in its tfvars.
# =============================================================================

resource "aws_s3_bucket" "mekhan_artifacts" {
  bucket = var.s3_bucket

  # Hetzner rejects bucket deletes when there are objects inside — explicit
  # protection so a `tofu destroy` doesn't half-succeed and leave the state
  # out of sync with reality. To actually delete, set this false, run apply,
  # then destroy.
  force_destroy = false

  lifecycle {
    prevent_destroy = true
  }
}
