# =============================================================================
# aithericon-executor Nomad job — split out of mekhan-service so it scales
# independently. Rendering pattern mirrors nomad_job "engine" in engine.tf.
# =============================================================================

resource "nomad_job" "executor" {
  jobspec = templatefile("${path.module}/executor.nomad.hcl.tpl", {
    namespace            = var.nomad_namespace
    datacenters          = jsonencode(var.nomad_datacenters)
    node_class           = var.node_class
    image                = "${var.executor_image_repository}:${var.image_tag}"
    image_tag            = var.image_tag
    registry_user        = var.registry_user
    registry_password    = var.registry_password
    nats_url             = var.nats_url
    s3_endpoint          = var.s3_endpoint
    s3_bucket            = var.s3_bucket
    s3_access_key        = var.s3_access_key
    s3_secret_key        = var.s3_secret_key
    cpu_mhz              = var.executor_cpu_mhz
    memory_mb            = var.executor_memory_mb
    executor_count       = var.executor_count
    executor_concurrency = var.executor_concurrency
    rust_log             = var.rust_log
  })

  purge_on_destroy = true
}
