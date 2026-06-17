# =============================================================================
# mekhan-service Nomad job spec — env-parameterized (dev | prod)
# =============================================================================
# Identical in deploy/dev and deploy/prod. The job ID, Consul service names,
# Traefik routers, Vault role/policies, and NATS creds path are all injected
# from locals.tf via main.tf so the two envs never collide on the shared
# cluster — see job "${job_id}" below.
#
# Templated by Terraform (main.tf). $${var} interpolations happen
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

job "${job_id}" {
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

        static = ${service_port}
        to     = ${service_port}
      }

      port "engine" {
        static = ${engine_service_port}
        to     = ${engine_service_port}
      }
    }

    service {
      name     = "${service_name}"
      port     = "http"
      provider = "consul"

      tags = [
        "mekhan",
        "traefik.enable=${traefik_enabled}",
        "traefik.http.routers.${router}.rule=Host(`${hostname}`)",
        "traefik.http.routers.${router}.entrypoints=websecure",
        "traefik.http.routers.${router}.tls=true",
        "traefik.http.routers.${router}.tls.certresolver=letsencrypt",
        "traefik.http.routers.${router}.service=${service_name}",
        # HTTP → HTTPS redirect
        "traefik.http.routers.${router_http}.rule=Host(`${hostname}`)",
        "traefik.http.routers.${router_http}.entrypoints=web",
        "traefik.http.routers.${router_http}.middlewares=https-redirect@file",
        "traefik.http.routers.${router_http}.service=${service_name}",
      ]

      check {
        type     = "http"
        path     = "/healthz"
        interval = "10s"
        timeout  = "2s"
      }
    }


    service {
      name     = "${engine_name}"
      port     = "engine"
      provider = "consul"

      tags = [
        "engine",
        "mekhan",
        "traefik.enable=true",
        "traefik.http.routers.${engine_router}.rule=Host(`${hostname}`) && PathPrefix(`/petri`)",
        "traefik.http.routers.${engine_router}.priority=200",
        "traefik.http.routers.${engine_router}.entrypoints=websecure",
        "traefik.http.routers.${engine_router}.tls=true",
        "traefik.http.routers.${engine_router}.tls.certresolver=letsencrypt",
        "traefik.http.routers.${engine_router}.middlewares=${engine_router}-stripprefix",
        "traefik.http.middlewares.${engine_router}-stripprefix.stripprefix.prefixes=/petri",
        "traefik.http.routers.${engine_router}.service=${engine_name}",
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

    # Authenticate to Vault using Nomad workload identity. The `${vault_role}`
    # JWT role + matching policies live in vault.tf and are bound to
    # nomad_job_id="${job_id}" + namespace="${namespace}". The policies
    # grant: (a) read on the NATS user creds path used below, (b) CRUD on
    # secret/data/aithericon/resources/* for VaultResourceStore (write side)
    # and the engine's secret-wrapping read side, (c) update on
    # sys/wrapping/wrap for cubbyhole response wrapping at job dispatch.
    vault {
      policies = ${vault_policies}
      role     = "${vault_role}"
    }

    task "service" {
      driver = "docker"

      # No registry auth — images come from the internal zot mirror
      # (zot.service.consul:5000), which the nodes trust anonymously and which
      # pull-through-caches from forge. Same convention as web-platform.
      config {
        image = "${image}"
        ports = ["http"]
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

      # Secret env vars rendered from Vault at alloc start (env = true), so the
      # VALUES never land in the rendered Nomad job — only the paths above do.
      # `runtime` = service-only secrets; `storage` = S3 keys (shared w/ executor).
      template {
        destination = "secrets/runtime.env"
        change_mode = "restart"
        env         = true
        data        = <<-EOH
{{- with secret "${runtime_secret_path}" }}
MEKHAN__DATABASE_URL={{ .Data.data.database_url }}
MEKHAN__AUTH__INTROSPECTION_CLIENT_SECRET={{ .Data.data.introspection_client_secret }}
MEKHAN__AUTH__BROKER_PAT={{ .Data.data.broker_pat }}
MEKHAN__EMAIL__SMTP_USERNAME={{ .Data.data.smtp_username }}
MEKHAN__EMAIL__SMTP_PASSWORD={{ .Data.data.smtp_password }}
{{- end }}
EOH
      }

      template {
        destination = "secrets/storage.env"
        change_mode = "restart"
        env         = true
        data        = <<-EOH
{{- with secret "${storage_secret_path}" }}
MEKHAN__S3__ACCESS_KEY={{ .Data.data.s3_access_key }}
MEKHAN__S3__SECRET_KEY={{ .Data.data.s3_secret_key }}
{{- end }}
EOH
      }

      env {
        MEKHAN__HOST          = "0.0.0.0"
        MEKHAN__PORT          = "${service_port}"
        MEKHAN__NATS_URL      = "${nats_url}"
        MEKHAN__NATS_CREDS    = "$${NOMAD_SECRETS_DIR}/nats.creds"
        MEKHAN__PETRI_LAB_URL = "${petri_lab_url}"
        MEKHAN__S3__ENDPOINT  = "${s3_endpoint}"
        MEKHAN__S3__BUCKET    = "${s3_bucket}"
        MEKHAN__AUTH__MODE                = "${auth_mode}"
        MEKHAN__AUTH__ISSUER_URL          = "${auth_issuer_url}"
        MEKHAN__AUTH__CLIENT_ID           = "${auth_client_id}"
        MEKHAN__AUTH__AUDIENCE            = "${auth_audience}"
        MEKHAN__AUTH__REDIRECT_URI        = "${auth_redirect_uri}"
        MEKHAN__AUTH__POST_LOGIN_REDIRECT = "${auth_post_login_redirect}"
        MEKHAN__AUTH__INTROSPECTION_CLIENT_ID = "${auth_introspection_client_id}"
        # Platform admins — comma-separated OIDC subjects/emails that get
        # `is_platform_admin`. REQUIRED in BFF mode to curate the shared platform
        # pool (mint the worker registration token, manage platform-tier infra);
        # dev_noop seeds its own admin so this is empty there. Comma-split by the
        # config loader (`auth.platform_admins` list key). Omitted when unset.
%{ if platform_admins != "" ~}
        MEKHAN__AUTH__PLATFORM_ADMINS     = "${platform_admins}"
%{ endif ~}
        # Invite-email delivery (Phase 4). mode=smtp makes the in-app invite
        # feature actually send the accept link via the relay; mode=log just
        # writes the link to the service log. PUBLIC_BASE_URL must be the
        # externally reachable origin so the accept link resolves for invitees.
        # (SMTP username/password come from the runtime.env template above.)
        MEKHAN__EMAIL__MODE            = "${email_mode}"
        MEKHAN__EMAIL__FROM_ADDRESS    = "${email_from_address}"
        MEKHAN__EMAIL__PUBLIC_BASE_URL = "${email_public_base_url}"
        MEKHAN__EMAIL__SMTP_HOST       = "${email_smtp_host}"
        MEKHAN__EMAIL__SMTP_PORT       = "${email_smtp_port}"
        MEKHAN__DEMOS__SEED            = "true"
        # Vault — VaultResourceStore writes resource version secrets to
        # secret/data/aithericon/resources/{ws}/{rid}/v{n}. Nomad's `vault {}`
        # stanza above already injects VAULT_TOKEN into the task env (workload-
        # identity exchange via the `mekhan-service` JWT role); VAULT_ADDR is
        # rendered here because Nomad doesn't propagate the client's vault.addr
        # to task env automatically. Without VAULT_ADDR set, service/src/main.rs
        # falls back to InMemoryResourceStore and logs a WARN — see the
        # `resource_store:` log line at boot.
        VAULT_ADDR = "${vault_addr}"
        RUST_LOG   = "${rust_log}"
      }

      resources {
        cpu    = ${cpu_mhz}
        memory = ${memory_mb}
      }
    }

    task "engine" {
      driver = "docker"

      # No registry auth — pulled from the internal zot mirror (see service task).
      config {
        image = "${engine_image}"
        ports = ["engine"]
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
    environment  = "${environment}"
    image_tag    = "${image_tag}"
    hostname     = "${hostname}"
  }
}
