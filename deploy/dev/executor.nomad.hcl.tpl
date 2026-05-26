# =============================================================================
# aithericon-executor Nomad job — dev
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

job "executor" {
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

    # Vault auth for THIS alloc covers one thing only: rendering the NATS
    # worker .creds bundle below. The `mekhan-executor` JWT role + the
    # `mekhan-nats-read` policy (both in deploy/dev/vault.tf) are deliberately
    # narrower than the mekhan-service role — the executor never reads or
    # writes secret/data/aithericon/resources/* by path, and never wraps. Its
    # secret-unwrap path uses the single-use wrapping token issued by the
    # engine as auth (vault_unwrap_secrets() does not consume VAULT_TOKEN).
    vault {
      policies = ["nomad-workloads", "mekhan-nats-read"]
      role     = "mekhan-executor"
    }

    task "executor" {
      driver = "docker"

      config {
        image = "${image}"
        # No port mapping — executor is NATS-driven, cancel HTTP is opt-in
        # via EXECUTOR_CANCEL__HTTP=true and not enabled here.
        auth {
          username = "${registry_user}"
          password = "${registry_password}"
        }
      }

      template {
        destination = "secrets/nats.creds"
        change_mode = "restart"
        perms       = "0644"
        data        = <<-EOH
{{- with secret "secret/data/nats/apps/mekhan/dev/worker" -}}
{{ .Data.data.creds }}
{{- end -}}
EOH
      }

      env {
        # Executor reads EXECUTOR_* (not MEKHAN__*) — see Dockerfile.executor
        # lines 175-185.
        EXECUTOR_NATS_URL       = "${nats_url}"
        EXECUTOR_NATS_CREDS     = "$${NOMAD_SECRETS_DIR}/nats.creds"
        # Must match the engine's EXECUTOR_NAMESPACE (set to "executor" in
        # engine.nomad.hcl.tpl). Default in the executor service is
        # "executor_jobs" — leaving it as default makes the executor listen on
        # subjects engine never publishes to. Symptom: automated steps stay
        # "pending" forever because dispatch messages sit in NATS unconsumed.
        EXECUTOR_NAMESPACE      = "executor"
        EXECUTOR_BASE_DIR       = "/var/lib/aithericon/executor"
        EXECUTOR_CONCURRENCY    = "${executor_concurrency}"
        EXECUTOR_PYTHON__ENABLED   = "true"
        EXECUTOR_PYTHON__PREFER_UV = "true"
        EXECUTOR_CANCEL__HTTP = "false"
        # S3 / object-storage backend for staging inputs (template scripts,
        # generated .pyi stubs) and outputs. MUST match what mekhan-service
        # uploads to. Symptom of mismatch: executor logs "staging failed:
        # artifact not found" because it's looking in a different bucket. The
        # double-underscore is config-rs's nesting separator.
        EXECUTOR_STORAGE__BACKEND                  = "s3"
        EXECUTOR_STORAGE__ENDPOINT                 = "${s3_endpoint}"
        EXECUTOR_STORAGE__BUCKET                   = "${s3_bucket}"
        EXECUTOR_STORAGE__REGION                   = "fsn1"
        EXECUTOR_STORAGE__CREDENTIALS__ACCESS_KEY  = "${s3_access_key}"
        EXECUTOR_STORAGE__CREDENTIALS__SECRET_KEY  = "${s3_secret_key}"
        # Vault — executor calls vault_unwrap_secrets() with the per-job
        # wrapping token as auth (X-Vault-Token: <wrapping>), so VAULT_TOKEN
        # is intentionally NOT used by the unwrap path. Only VAULT_ADDR is
        # needed; staging.rs:645 reads it directly. Symptom if unset: jobs
        # with wrapped secrets stage with `{{secret:...}}` refs unresolved
        # and the underlying script sees the literal placeholder.
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
    environment = "dev"
    image_tag   = "${image_tag}"
  }
}
