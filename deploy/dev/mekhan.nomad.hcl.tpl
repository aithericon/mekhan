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

      port "engine" {
        static = ${engine_service_port}
        to     = ${engine_service_port}
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
        path     = "/api/v1/health"
        interval = "10s"
        timeout  = "2s"
      }
    }


    service {
      name     = "engine"
      port     = "engine"
      provider = "consul"

      tags = [
        "engine",
        "mekhan",
        "traefik.enable=true",
        "traefik.http.routers.engine.rule=Host(`${hostname}`) && PathPrefix(`/petri`)",
        "traefik.http.routers.engine.priority=200",
        "traefik.http.routers.engine.entrypoints=websecure",
        "traefik.http.routers.engine.tls=true",
        "traefik.http.routers.engine.tls.certresolver=letsencrypt",
        "traefik.http.routers.engine.middlewares=engine-stripprefix",
        "traefik.http.middlewares.engine-stripprefix.stripprefix.prefixes=/petri",
        "traefik.http.routers.engine.service=engine",
      ]

      # TCP check rather than HTTP — the engine doesn't expose /health
      # (all routes are /api/*, per engine/core-engine/crates/api/src/router.rs).
      check {
        type     = "tcp"
        port     = "engine"
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

    # Authenticate to Vault using Nomad workload identity. The `mekhan-service`
    # JWT role + matching policies live in deploy/dev/vault.tf and are bound to
    # nomad_job_id="mekhan-service" + namespace="${namespace}". The policies
    # grant: (a) read on the NATS user creds path used below, (b) CRUD on
    # secret/data/aithericon/resources/* for VaultResourceStore (write side)
    # and the engine's secret-wrapping read side, (c) update on
    # sys/wrapping/wrap for cubbyhole response wrapping at job dispatch.
    vault {
      policies = ["nomad-workloads", "mekhan-nats-read", "mekhan-resources-rw", "mekhan-wrap"]
      role     = "mekhan-service"
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
        MEKHAN__AUTH__POST_LOGIN_REDIRECT = "${auth_post_login_redirect}"
        MEKHAN__AUTH__INTROSPECTION_CLIENT_ID     = "${auth_introspection_client_id}"
        MEKHAN__AUTH__INTROSPECTION_CLIENT_SECRET = "${auth_introspection_client_secret}"
        MEKHAN__AUTH__BROKER_PAT                  = "${auth_broker_pat}"
        MEKHAN__DEMOS__SEED        = "true"
        # Vault — VaultResourceStore writes resource version secrets to
        # secret/data/aithericon/resources/{ws}/{rid}/v{n}. Nomad's `vault {}`
        # stanza above already injects VAULT_TOKEN into the task env (workload-
        # identity exchange via the `mekhan-service` JWT role); VAULT_ADDR is
        # rendered here because Nomad doesn't propagate the client's vault.addr
        # to task env automatically. Without VAULT_ADDR set, service/src/main.rs
        # falls back to InMemoryResourceStore and logs a WARN — see the
        # `resource_store:` log line at boot.
        VAULT_ADDR                 = "${vault_addr}"
        RUST_LOG                   = "${rust_log}"
      }

      resources {
        cpu    = ${cpu_mhz}
        memory = ${memory_mb}
      }
    }

    task "engine" {
      driver = "docker"

      config {
        image = "${engine_image}"
        ports = ["engine"]

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

        PORT                = "${engine_service_port}"
        NATS_URL            = "${nats_url}"
        NATS_CREDS          = "$${NOMAD_SECRETS_DIR}/nats.creds"
        EXECUTOR_NATS_URL   = "${nats_url}"
        EXECUTOR_NATS_CREDS = "$${NOMAD_SECRETS_DIR}/nats.creds"
        EXECUTOR_ENABLED    = "true"
        EXECUTOR_NAMESPACE  = "executor"
        # Vault — engine resolves `{{secret:...}}` refs against VaultSecretStore
        # and wraps resolved values into a single-use token before publishing
        # on NATS. Needs VAULT_TOKEN (Nomad-injected via the workload-identity
        # exchange) + VAULT_ADDR (templated here). The `mekhan-wrap` policy on
        # the mekhan-service role grants the `sys/wrapping/wrap` update cap;
        # `mekhan-resources-rw` grants the read on secret/data/aithericon/
        # resources/* that the engine needs to fetch values before wrapping.
        VAULT_ADDR          = "${vault_addr}"
        RUST_LOG            = "${rust_log}"
      }

      resources {
        cpu    = ${engine_cpu_mhz}
        memory = ${engine_memory_mb}
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
