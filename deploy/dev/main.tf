# =============================================================================
# Dev deploy — mekhan-service Nomad job
# =============================================================================
# Single resource: a Nomad job rendered from mekhan.nomad.hcl.tpl. The image
# tag flows in via var.image_tag (CI sets it to the commit SHA), so a re-apply
# with a new tag triggers Nomad's rolling update.

# =============================================================================

resource "nomad_job" "mekhan_service" {
  depends_on = [
    postgresql_database.mekhan_dev,
    # The job's template stanzas read these at alloc start — write them first.
    vault_kv_secret_v2.mekhan_runtime,
    vault_kv_secret_v2.mekhan_storage,
    # …and the read grants must exist before the workload-identity token reads
    # them (the vault {} policies are passed as strings, so TF can't infer this).
    vault_policy.mekhan_nats_read,
    vault_policy.mekhan_resources_rw,
  ]

  jobspec = templatefile("${path.module}/mekhan.nomad.hcl.tpl", {
    namespace         = var.nomad_namespace
    environment       = var.environment
    job_id            = local.service_job_id
    service_name      = local.service_consul_name
    engine_name       = local.engine_consul_name
    router            = local.traefik_router
    router_http       = local.traefik_router_http
    engine_router     = local.engine_router
    vault_role        = local.vault_role_service
    vault_policies    = jsonencode(local.service_vault_policies)
    nats_user_kv_path = local.nats_user_kv_path
    # Secret VALUES are NOT passed into the jobspec — the `template` stanzas in
    # mekhan.nomad.hcl.tpl read them from Vault at alloc start. Only these PATHS
    # appear in the rendered Nomad job.
    runtime_secret_path    = local.runtime_secret_read_path
    storage_secret_path    = local.storage_secret_read_path
    datacenters            = jsonencode(var.nomad_datacenters)
    node_class             = var.node_class
    image                  = "${var.image_repository}:${var.image_tag}"
    image_tag              = var.image_tag
    service_port           = var.service_port
    cpu_mhz                = var.cpu_mhz
    memory_mb              = var.memory_mb
    hostname               = var.hostname
    traefik_enabled        = var.traefik_enabled
    nats_url               = var.nats_url
    runner_nats_public_url = var.runner_nats_public_url
    vault_addr             = var.vault_addr
    petri_lab_url          = local.petri_lab_url
    s3_endpoint            = var.s3_endpoint
    s3_bucket              = var.s3_bucket
    auth_mode              = var.auth_mode
    rust_log               = var.rust_log

    auth_issuer_url = var.zitadel_issuer_url
    auth_client_id  = zitadel_application_oidc.spa.client_id
    auth_audience   = zitadel_application_oidc.spa.client_id

    auth_redirect_uri = "https://${var.hostname}/api/auth/callback"

    auth_post_login_redirect = "https://${var.hostname}/"

    auth_introspection_client_id = zitadel_application_api.introspect.client_id

    # Comma-joined for the single MEKHAN__AUTH__PLATFORM_ADMINS env var (the
    # config loader splits it back into a list). Empty string ⇒ the jobspec omits
    # the var entirely.
    platform_admins = join(",", var.platform_admins)

    email_mode            = var.email_mode
    email_from_address    = var.email_from_address
    email_public_base_url = "https://${var.hostname}"
    email_smtp_host       = var.email_smtp_host
    email_smtp_port       = var.email_smtp_port


    engine_image        = "${var.engine_image_repository}:${var.image_tag}"
    engine_service_port = var.engine_service_port
    engine_cpu_mhz      = var.engine_cpu_mhz
    engine_memory_mb    = var.engine_memory_mb
  })

  purge_on_destroy = true
}
