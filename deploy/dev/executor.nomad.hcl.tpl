# =============================================================================
# aithericon-executor Nomad job — env-parameterized (dev | prod)
# =============================================================================
# Identical in deploy/dev and deploy/prod. Job ID, Vault role/policies, NATS
# creds path, reg-token path, and the mekhan-service URL are injected from
# locals.tf via executor.tf so the two envs never collide on the shared cluster.
# =============================================================================
# Standalone Nomad job, split out of mekhan-service so executor concurrency can
# scale independently of the BFF. The executor is NATS-driven (work-pickup over
# JetStream), so there is no network port, no Consul service registration, and
# no Traefik routing — mekhan-service and the engine reach it only via NATS
# subjects.
#
# Co-location with `service` is no longer required: communication has always
# gone via NATS, never directly across the shared alloc network namespace.
# =============================================================================

job "${job_id}" {
  namespace   = "${namespace}"
  datacenters = ${datacenters}
  type        = "service"

  group "executor" {
    count = ${executor_count}

    constraint {
      attribute = "$${node.class}"
      value     = "${node_class}"
    }

    restart {
      attempts = 3
      delay    = "15s"
      interval = "5m"
      mode     = "delay"
    }

    vault {
      policies = ${vault_policies}
      role     = "${vault_role}"
    }

    task "executor" {
      driver = "docker"

      # No registry auth — pulled from the internal zot mirror (anonymous).
      config {
        image = "${image}"
        # No port mapping — executor is NATS-driven, cancel HTTP is opt-in
        # via EXECUTOR_CANCEL__HTTP=true and not enabled here.
      }

      template {
        destination = "secrets/nats.creds"
        change_mode = "restart"
        perms       = "0644"
        data        = <<-EOH
{{- with secret "secret/data/${nats_user_kv_path}" -}}
{{ .Data.data.creds }}
{{- end -}}
EOH
      }

      # Worker-pool enrollment secret (self-enroll on boot → routing_partition +
      # worker bearer). IMPORTANT (platform-tier): the `default` worker group
      # lives in the shared PLATFORM scope, not a workspace — so this MUST be a
      # PLATFORM-scoped token, minted with {"group":"default","platform":true} by
      # a platform admin (see platform_admins). A workspace-scoped token enrolls
      # with HTTP 400 "worker group 'default' does not resolve to a worker
      # `capacity` resource in this workspace". Re-mint + `vault kv put` (see
      # deploy/README.md) and restart this job after the platform-tier migration.
      template {
        destination = "secrets/reg-token.env"
        change_mode = "restart"
        env         = true
        data        = <<-EOH
{{- with secret "secret/data/${reg_token_path}" -}}
EXECUTOR_WORKER_REG_TOKEN={{ .Data.data.worker_reg_token }}
{{- end -}}
EOH
      }

      # S3 artifact-store keys from Vault at alloc start (env = true) — values
      # never appear in the rendered job. Shared `storage` KV with the service.
      template {
        destination = "secrets/storage.env"
        change_mode = "restart"
        env         = true
        data        = <<-EOH
{{- with secret "${storage_secret_path}" }}
EXECUTOR_STORAGE__CREDENTIALS__ACCESS_KEY={{ .Data.data.s3_access_key }}
EXECUTOR_STORAGE__CREDENTIALS__SECRET_KEY={{ .Data.data.s3_secret_key }}
{{- end }}
EOH
      }

      env {
        # Executor reads EXECUTOR_* (not MEKHAN__*) — see Dockerfile.executor
        # lines 175-185.
        EXECUTOR_NATS_URL       = "${nats_url}"
        EXECUTOR_NATS_CREDS     = "$${NOMAD_SECRETS_DIR}/nats.creds"

        # Boot-time worker enrollment endpoint — mekhan-service's stable
        # in-cluster address (static port). Paired with EXECUTOR_WORKER_REG_TOKEN
        # (rendered from Vault above).
        EXECUTOR_MEKHAN_URL     = "http://${mekhan_service_name}.service.consul:${service_port}"

        EXECUTOR_NAMESPACE      = "executor"
        EXECUTOR_BASE_DIR       = "/var/lib/aithericon/executor"
        EXECUTOR_CONCURRENCY    = "${executor_concurrency}"
        EXECUTOR_PYTHON__ENABLED   = "true"
        EXECUTOR_PYTHON__PREFER_UV = "true"
        EXECUTOR_CANCEL__HTTP = "false"

        EXECUTOR_STORAGE__BACKEND  = "s3"
        EXECUTOR_STORAGE__ENDPOINT = "${s3_endpoint}"
        EXECUTOR_STORAGE__BUCKET   = "${s3_bucket}"
        EXECUTOR_STORAGE__REGION   = "fsn1"

        VAULT_ADDR            = "${vault_addr}"
        RUST_LOG              = "${rust_log}"
      }

      resources {
        cpu    = ${cpu_mhz}
        memory = ${memory_mb}
      }
    }
  }

  meta {
    project     = "mekhan"
    environment = "${environment}"
    image_tag   = "${image_tag}"
  }
}
