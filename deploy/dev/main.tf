# =============================================================================
# Dev deploy — mekhan-service Nomad job
# =============================================================================
# Single resource: a Nomad job rendered from mekhan.nomad.hcl.tpl. The image
# tag flows in via var.image_tag (CI sets it to the commit SHA), so a re-apply
# with a new tag triggers Nomad's rolling update.

# =============================================================================

resource "nomad_job" "mekhan_service" {
  # Make sure the role + database exist before the Nomad job tries to use the
  # DSN. Without this, on a clean apply Nomad would receive a URL pointing at
  # credentials that don't exist yet (race against the postgresql provider).
  depends_on = [
    postgresql_database.mekhan_dev,
  ]

  jobspec = templatefile("${path.module}/mekhan.nomad.hcl.tpl", {
    namespace         = var.nomad_namespace
    datacenters       = jsonencode(var.nomad_datacenters)
    node_class        = var.node_class
    image             = "${var.image_repository}:${var.image_tag}"
    image_tag         = var.image_tag
    registry_user     = var.registry_user
    registry_password = var.registry_password
    service_port      = var.service_port
    cpu_mhz           = var.cpu_mhz
    memory_mb         = var.memory_mb
    hostname          = var.hostname
    traefik_enabled   = var.traefik_enabled
    database_url      = local.database_url
    nats_url          = var.nats_url
    petri_lab_url     = var.petri_lab_url
    s3_endpoint       = var.s3_endpoint
    s3_bucket         = var.s3_bucket
    s3_access_key     = var.s3_access_key
    s3_secret_key     = var.s3_secret_key
    auth_mode         = var.auth_mode
    rust_log          = var.rust_log
  })

  purge_on_destroy = true
}
