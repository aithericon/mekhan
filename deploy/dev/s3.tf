# =============================================================================
# mekhan-service artifact bucket — Hetzner Object Storage


resource "aws_s3_bucket" "mekhan_artifacts" {
  bucket = var.s3_bucket
  force_destroy = false

  lifecycle {
    prevent_destroy = true
  }
}
