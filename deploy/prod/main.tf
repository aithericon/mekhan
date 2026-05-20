# =============================================================================
# Prod deploy — mekhan-service Nomad job
# =============================================================================
# Mirrors deploy/dev/main.tf. Threads service_count into the template so prod
# runs multiple replicas. The mekhan.nomad.hcl.tpl in this folder is its own
# copy (vs. dev's) so prod-only changes don't sneak into dev.
# =============================================================================

resource "nomad_job" "mekhan_service" {
  jobspec = templatefile("${path.module}/mekhan.nomad.hcl.tpl", {
    namespace         = var.nomad_namespace
    datacenters       = jsonencode(var.nomad_datacenters)
    node_class        = var.node_class
    image             = "${var.image_repository}:${var.image_tag}"
    image_tag         = var.image_tag
    registry_user     = var.registry_user
    registry_password = var.registry_password
    service_count     = var.service_count
    service_port      = var.service_port
    cpu_mhz           = var.cpu_mhz
    memory_mb         = var.memory_mb
    hostname          = var.hostname
    traefik_enabled   = var.traefik_enabled
    database_url      = var.database_url
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
