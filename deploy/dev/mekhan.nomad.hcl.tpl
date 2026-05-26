# =============================================================================
# mekhan-service Nomad job spec — dev
# =============================================================================
# Templated by Terraform (deploy/dev/main.tf). $${var} interpolations happen
# at terraform plan time; literal $${NOMAD_*} env refs that survive into Nomad
# are escaped with a leading backslash where needed (none here today).
#
# Patterns mirrored from HetznerCluster/layers/06k_hookshot/jobs/hookshot.nomad.hcl
# (representative stateless HTTP service in this cluster):
#   - node-class constraint pinning to the right pool
#   - provider = "consul" so Traefik (which discovers via Consul Catalog) can route
#   - Traefik tags for HTTPS + HTTP→HTTPS redirect via the cluster's ACME resolver
#   - meta block for operational visibility
#   - update.canary + auto_revert for safe rolling deploys
# =============================================================================

job "mekhan-service" {
  namespace   = "${namespace}"
  datacenters = ${datacenters}
  type        = "service"

  update {
    max_parallel      = 1
    canary            = 1
    health_check      = "checks"
    min_healthy_time  = "30s"
    healthy_deadline  = "3m"
    progress_deadline = "5m"
    auto_revert       = true
    auto_promote      = true
  }

  group "mekhan" {
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
      name     = "mekhan-service"
      port     = "http"
      provider = "consul"

      tags = [
        "mekhan",
        "traefik.enable=${traefik_enabled}",
        "traefik.http.routers.mekhan.rule=Host(`${hostname}`)",
        "traefik.http.routers.mekhan.entrypoints=websecure",
        "traefik.http.routers.mekhan.tls=true",
        "traefik.http.routers.mekhan.tls.certresolver=letsencrypt",
        "traefik.http.routers.mekhan.service=mekhan-service",
        # HTTP → HTTPS redirect
        "traefik.http.routers.mekhan-http.rule=Host(`${hostname}`)",
        "traefik.http.routers.mekhan-http.entrypoints=web",
        "traefik.http.routers.mekhan-http.middlewares=https-redirect@file",
        "traefik.http.routers.mekhan-http.service=mekhan-service",
      ]

      check {
        type     = "http"
        path     = "/api/health"
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

    # Authenticate to Vault using Nomad workload identity. The `mekhan-dev`
    # JWT role + matching policy live in deploy/dev/nats.tf (this repo) and
    # are bound to nomad_job_id="mekhan-service" + namespace="default";
    # together they grant read on the NATS user creds path below.
    vault {
      policies = ["nomad-workloads", "mekhan-dev"]
      role     = "mekhan-dev"
    }

    task "service" {
      driver = "docker"

      config {
        image = "${image}"
        ports = ["http"]

        auth {
          username = "${registry_user}"
          password = "${registry_password}"
        }
      }

      # NATS user credentials, rendered from Vault at alloc-start. The bundle
      # is provisioned by deploy/dev/scripts/generate-nats-user.sh (run once
      # in CI before first apply, re-run to rotate) and lives at
      # secret/nats/apps/mekhan/dev/worker.
      # change_mode=restart so a creds rotation cycles the task automatically.
      template {
        destination = "secrets/nats.creds"
        change_mode = "restart"
        # 0644 not 0600 — Nomad's template runs as root on the client; the
        # container task user (UID 1000, per Dockerfile.service.prebuilt)
        # can't read root-owned 0600 files. The alloc's secrets dir is
        # already private (mounted only into this task's namespace) so
        # world-readable inside the container is fine.
        perms       = "0644"
        data        = <<-EOH
{{- with secret "secret/data/nats/apps/mekhan/dev/worker" -}}
{{ .Data.data.creds }}
{{- end -}}
EOH
      }

      env {
        MEKHAN__HOST           = "0.0.0.0"
        MEKHAN__PORT           = "${service_port}"
        MEKHAN__DATABASE_URL   = "${database_url}"
        MEKHAN__NATS_URL       = "${nats_url}"
        MEKHAN__NATS_CREDS     = "$${NOMAD_SECRETS_DIR}/nats.creds"
        MEKHAN__PETRI_LAB_URL  = "${petri_lab_url}"
        MEKHAN__S3__ENDPOINT   = "${s3_endpoint}"
        MEKHAN__S3__BUCKET     = "${s3_bucket}"
        MEKHAN__S3__ACCESS_KEY = "${s3_access_key}"
        MEKHAN__S3__SECRET_KEY = "${s3_secret_key}"
        MEKHAN__AUTH__MODE         = "${auth_mode}"
        MEKHAN__AUTH__ISSUER_URL   = "${auth_issuer_url}"
        MEKHAN__AUTH__CLIENT_ID    = "${auth_client_id}"
        MEKHAN__AUTH__AUDIENCE     = "${auth_audience}"
        MEKHAN__AUTH__REDIRECT_URI = "${auth_redirect_uri}"
        # Used by handlers.rs for both the post-login bounce AND the
        # `post_logout_redirect_uri` mekhan hands to Zitadel's end_session
        # endpoint. The latter requires an exact match with one of the
        # post_logout_redirect_uris we registered (see zitadel.tf), and
        # Zitadel only allows absolute URLs — so we override the default `/`.
        MEKHAN__AUTH__POST_LOGIN_REDIRECT = "${auth_post_login_redirect}"
        # Seed the built-in demo templates baked into the image at /app/demos
        # (Dockerfile.service.prebuilt COPYs the demos/ folder + ENV sets
        # MEKHAN__DEMOS__DIR=/app/demos). Seeder runs once on startup before
        # the HTTP listener accepts requests; idempotent by templateId, so
        # leaving this true across redeploys is safe.
        MEKHAN__DEMOS__SEED        = "true"
        RUST_LOG                   = "${rust_log}"
      }

      resources {
        cpu    = ${cpu_mhz}
        memory = ${memory_mb}
      }
    }
  }

  meta {
    project      = "mekhan"
    environment  = "dev"
    image_tag    = "${image_tag}"
    hostname     = "${hostname}"
  }
}
