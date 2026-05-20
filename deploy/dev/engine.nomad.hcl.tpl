# =============================================================================
# aithericon-engine Nomad job — dev
# =============================================================================
# Standalone Nomad job (NOT a sibling task of mekhan-service). mekhan-service
# reaches it at engine.service.consul:3030 via Consul DNS, so the engine must
# register itself in Consul under that exact service name — that's what the
# `service { name = "engine" }` block below does.
#
# No DB, no Vault secrets — engine only needs NATS (rendered from the same
# Vault path mekhan-service uses; the engine's NATS account permissions are
# provisioned by HetznerCluster's 10_mekhan_nats layer).
# =============================================================================

job "engine" {
  region      = "global"
  datacenters = ${datacenters}
  namespace   = "${namespace}"
  type        = "service"

  group "engine" {
    count = 1

    constraint {
      attribute = "$${node.class}"
      value     = "${node_class}"
    }

    network {
      port "http" {
        to = ${service_port}
      }
    }

    service {
      name     = "engine"
      port     = "http"
      provider = "consul"

      tags = [
        "engine",
        "mekhan",
      ]

      # TCP check rather than HTTP because the engine doesn't expose a
      # dedicated /health endpoint (per engine/core-engine/crates/api/src/
      # router.rs). All routes are /api/*; TCP probes liveness without
      # masking application-level errors.
      check {
        type     = "tcp"
        interval = "10s"
        timeout  = "2s"
      }
    }

    restart {
      attempts = 3
      delay    = "15s"
      interval = "5m"
      mode     = "delay"
    }

    # Same NATS account as mekhan-service / executor — engine subscribes to
    # the workflow-event JetStream and publishes execution events. The
    # `mekhan-dev` Vault role + policy live in deploy/dev/nats.tf (this
    # repo); its bound_claims list both "mekhan-service" and "engine" as
    # acceptable nomad_job_id values, so both jobs assume the same role.
    vault {
      policies = ["nomad-workloads", "mekhan-dev"]
      role     = "mekhan-dev"
    }

    task "engine" {
      driver = "docker"

      config {
        image = "${image}"
        ports = ["http"]

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
        # Engine reads PORT + NATS_URL + EXECUTOR_* — see just/dev.just:268-275
        # for the canonical local-dev invocation. Same env-var shape we use
        # here; only the values differ (consul DNS vs localhost).
        PORT                = "${service_port}"
        NATS_URL            = "${nats_url}"
        NATS_CREDS          = "$${NOMAD_SECRETS_DIR}/nats.creds"
        EXECUTOR_NATS_URL   = "${nats_url}"
        EXECUTOR_NATS_CREDS = "$${NOMAD_SECRETS_DIR}/nats.creds"
        EXECUTOR_ENABLED    = "true"
        EXECUTOR_NAMESPACE  = "executor"
        RUST_LOG            = "${rust_log}"
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
