# =============================================================================
# aithericon-engine Nomad job — separate job from mekhan-service
# =============================================================================
# Rendering pattern mirrors nomad_job "mekhan" in main.tf — just simpler
# (engine has no DB, no Postgres provider dependency).
# =============================================================================

resource "nomad_job" "engine" {
  jobspec = templatefile("${path.module}/engine.nomad.hcl.tpl", {
    namespace         = var.nomad_namespace
    datacenters       = jsonencode(var.nomad_datacenters)
    node_class        = var.node_class
    image             = "${var.engine_image_repository}:${var.image_tag}"
    image_tag         = var.image_tag
    registry_user     = var.registry_user
    registry_password = var.registry_password
    service_port      = var.engine_service_port
    nats_url          = var.nats_url
    cpu_mhz           = var.engine_cpu_mhz
    memory_mb         = var.engine_memory_mb
    rust_log          = var.rust_log
  })

  purge_on_destroy = true
}
