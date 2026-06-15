# =============================================================================
# aithericon-executor Nomad job — split out of mekhan-service so it scales
# independently. Rendering pattern mirrors nomad_job "engine" in engine.tf.
# =============================================================================

resource "nomad_job" "executor" {
  # The job's storage.env template reads this at alloc start — write it first.
  depends_on = [vault_kv_secret_v2.mekhan_storage]

  jobspec = templatefile("${path.module}/executor.nomad.hcl.tpl", {
    namespace           = var.nomad_namespace
    environment         = var.environment
    job_id              = local.executor_job_id
    mekhan_service_name = local.service_consul_name
    vault_role          = local.vault_role_executor
    vault_policies      = jsonencode(local.executor_vault_policies)
    nats_user_kv_path   = local.nats_user_kv_path
    reg_token_path      = local.executor_reg_token_path
    datacenters         = jsonencode(var.nomad_datacenters)
    node_class          = var.node_class
    image               = "${var.executor_image_repository}:${var.image_tag}"
    image_tag           = var.image_tag
    nats_url            = var.nats_url
    vault_addr          = var.vault_addr

    service_port = var.service_port
    s3_endpoint  = var.s3_endpoint
    s3_bucket    = var.s3_bucket
    # S3 keys come from Vault at runtime via the storage.env template stanza —
    # only this path appears in the rendered job, not the key values.
    storage_secret_path  = local.storage_secret_read_path
    cpu_mhz              = var.executor_cpu_mhz
    memory_mb            = var.executor_memory_mb
    executor_count       = var.executor_count
    executor_concurrency = var.executor_concurrency
    rust_log             = var.rust_log
  })

  purge_on_destroy = true
}
