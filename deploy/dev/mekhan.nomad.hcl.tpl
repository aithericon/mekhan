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
    # A fresh deploy always ships a new image tag (= commit SHA), so zot has a
    # cold cache and pull-through-fetches every layer from Forgejo on demand.
    # That first pull can blow past the docker driver's 5m image_pull_timeout
    # ("context deadline exceeded") and only succeeds once zot has warmed its
    # blob cache across a few restart attempts. The alloc runs TWO tasks
    # (service + engine) that pull SEQUENTIALLY on the same node, so the cold-
    # pull cost is additive (~4.5m + ~6m observed) and overran the old 10m
    # healthy_deadline by ~40s — failing an otherwise-fine deploy. Widened to
    # 20m/30m so the serial cold pull of both images can't trip auto_revert.
    healthy_deadline  = "20m"
    progress_deadline = "30m"
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
      # More in-place pull attempts within the (now wider) progress_deadline so
      # a cold zot pull-through gets several shots at warming before the deploy
      # is declared failed. interval spans the whole window so attempts aren't
      # capped early.
      attempts = 5
      delay    = "15s"
      interval = "20m"
      mode     = "delay"
    }

  
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
        # Cold zot pull-through from Forgejo on a fresh tag can far exceed the
        # docker driver's default 5m pull timeout ("context deadline exceeded").
        # Give a single pull room to complete instead of aborting mid-fetch.
        image_pull_timeout = "15m"
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
# Headless provisioning (see bootstrap.tf): a platform root token for automated
# platform-admin ops, and platform-scoped bootstrap registration tokens the
# startup seeder upserts so the executor/runners self-enroll with no mint.
MEKHAN__AUTH__PLATFORM_ROOT_TOKEN={{ .Data.data.platform_root_token }}
MEKHAN__BOOTSTRAP__WORKER_REGISTRATION_TOKEN={{ .Data.data.bootstrap_worker_reg_token }}
MEKHAN__BOOTSTRAP__RUNNER_REGISTRATION_TOKEN={{ .Data.data.bootstrap_runner_reg_token }}
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

      # Runner-signing key (mekhan zero-secret enrollment). The `signing_seed` is
      # a signing key OF the mekhan-<env> NATS account, listed in the pushed
      # account JWT — so scoped runner JWTs mekhan mints with it are trusted by
      # the resolver. Rendered as an env var (env = true) so it wins over
      # RunnerNatsSigner's local-file / auto-generate fallbacks; without it
      # mekhan auto-generates an untrusted account key and every runner connect
      # fails `authorization violation`.
      template {
        destination = "secrets/runner-signing.env"
        change_mode = "restart"
        env         = true
        data        = <<-EOH
{{- with secret "secret/data/${nats_account_kv_path}" }}
RUNNERS_NATS_SIGNING_SEED={{ .Data.data.signing_seed }}
RUNNERS_NATS_ACCOUNT_ID={{ .Data.data.public_key }}
{{- end }}
EOH
      }

      env {
        MEKHAN__HOST          = "0.0.0.0"
        MEKHAN__PORT          = "${service_port}"
        MEKHAN__NATS_URL      = "${nats_url}"
        MEKHAN__NATS_CREDS    = "$${NOMAD_SECRETS_DIR}/nats.creds"
        # Public WebSocket front door advertised to enrolled external runners so
        # a bare daemon needs no EXECUTOR_NATS_URL. Distinct from MEKHAN__NATS_URL
        # (mekhan's own internal mesh connection). Dev shares prod's NATS, so this
        # is the same `wss://nats.aithericon.eu` host.
        MEKHAN__RUNNER_NATS_PUBLIC_URL = "${runner_nats_public_url}"
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

        MEKHAN__EMAIL__MODE            = "${email_mode}"
        MEKHAN__EMAIL__FROM_ADDRESS    = "${email_from_address}"
        MEKHAN__EMAIL__PUBLIC_BASE_URL = "${email_public_base_url}"
        MEKHAN__EMAIL__SMTP_HOST       = "${email_smtp_host}"
        MEKHAN__EMAIL__SMTP_PORT       = "${email_smtp_port}"
        MEKHAN__DEMOS__SEED            = "true"

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
        # See the service task: cold zot pull-through can exceed docker's default
        # 5m pull timeout. Widen it so a slow first pull doesn't abort.
        image_pull_timeout = "15m"
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
