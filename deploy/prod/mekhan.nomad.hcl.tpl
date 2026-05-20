# =============================================================================
# mekhan-service Nomad job spec — prod
# =============================================================================
# Differences vs. dev/mekhan.nomad.hcl.tpl:
#   - count = ${service_count}  (multiple replicas)
#   - MEKHAN_ENV=prod forces main.rs's refusal to start in dev_noop mode
#
# Pattern mirrors HetznerCluster/layers/06k_hookshot/jobs/hookshot.nomad.hcl.
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
    count = ${service_count}

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

      env {
        MEKHAN__HOST           = "0.0.0.0"
        MEKHAN__PORT           = "${service_port}"
        MEKHAN__DATABASE_URL   = "${database_url}"
        MEKHAN__NATS_URL       = "${nats_url}"
        MEKHAN__PETRI_LAB_URL  = "${petri_lab_url}"
        MEKHAN__S3__ENDPOINT   = "${s3_endpoint}"
        MEKHAN__S3__BUCKET     = "${s3_bucket}"
        MEKHAN__S3__ACCESS_KEY = "${s3_access_key}"
        MEKHAN__S3__SECRET_KEY = "${s3_secret_key}"
        MEKHAN__AUTH__MODE     = "${auth_mode}"
        MEKHAN_ENV             = "prod"
        RUST_LOG               = "${rust_log}"
      }

      resources {
        cpu    = ${cpu_mhz}
        memory = ${memory_mb}
      }
    }
  }

  meta {
    project      = "mekhan"
    environment  = "prod"
    image_tag    = "${image_tag}"
    hostname     = "${hostname}"
  }
}
