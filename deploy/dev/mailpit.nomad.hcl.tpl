# =============================================================================
# mailpit Nomad job — dev deployment
# =============================================================================
# Cluster SMTP capture server — the deploy-time mirror of `just dev mailhog-up`.
# Catches every message the SMTP executor backend sends and exposes them in a
# web UI. No real delivery, no credentials — same role as the local container,
# so the `mail` resource works in-cluster exactly like it does on a laptop.
#
# Mailpit (not MailHog) because the cluster nodes are arm64 and MailHog's image
# is amd64-only; Mailpit is a multi-arch drop-in (same SMTP 1025 / UI 8025).
#
#   SMTP : mailhog.service.consul:1025          → point the `mail` resource here
#   UI   : https://${hostname}/mail             → browse captured messages
#
# Same render/wire pattern as nomad_job "executor" in executor.tf.
# =============================================================================

job "mailpit" {
  namespace   = "${namespace}"
  datacenters = ${datacenters}
  type        = "service"

  group "mailpit" {
    count = 1

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

    network {
      # Host mode (like loki/alertmanager) so the static ports bind on the
      # NODE IP and Consul registers a routable host address. With the default
      # (non-host) mode, the service registers the alloc IP while the port is
      # forwarded on the host → cross-node connects get "connection refused".
      mode = "host"

      # Static SMTP port so the `mail` resource can target a stable host:port
      # (the smtp resource has no SRV/dynamic-port resolution).
      port "smtp" {
        static = ${smtp_port}
      }
      port "ui" {
        static = 8025
      }
    }

    task "mailpit" {
      driver = "docker"

      config {
        image        = "${image}"
        network_mode = "host"
      }

      env {
        MP_SMTP_BIND_ADDR = "0.0.0.0:1025"
        MP_UI_BIND_ADDR   = "0.0.0.0:8025"
        # Served under /mail on the mekhan host (see Traefik tags) — webroot
        # makes Mailpit emit correct asset URLs without a dedicated DNS name.
        MP_WEBROOT = "/mail"
      }

      resources {
        cpu    = ${cpu_mhz}
        memory = ${memory_mb}
      }
    }

    # SMTP receiver — what the executor's `mail` resource connects to.
    # Service name "mailhog" so the resource host reads naturally and the
    # catcher can be swapped later without touching the resource binding.
    service {
      name     = "mailhog"
      port     = "smtp"
      provider = "consul"
      tags     = ["smtp", "mail", "capture"]

      check {
        type     = "tcp"
        port     = "smtp"
        interval = "15s"
        timeout  = "3s"
      }
    }

    # Web UI on the existing mekhan host under /mail (same Host+PathPrefix
    # pattern the engine uses for /petri — no new DNS record needed).
    service {
      name     = "mailpit-ui"
      port     = "ui"
      provider = "consul"
      tags = [
        "traefik.enable=true",
        "traefik.http.routers.mailpit.rule=Host(`${hostname}`) && PathPrefix(`/mail`)",
        "traefik.http.routers.mailpit.priority=200",
        "traefik.http.routers.mailpit.entrypoints=websecure",
        "traefik.http.routers.mailpit.tls=true",
        "traefik.http.routers.mailpit.tls.certresolver=letsencrypt",
        "traefik.http.routers.mailpit.service=mailpit-ui",
      ]

      check {
        type     = "http"
        path     = "/mail"
        port     = "ui"
        interval = "15s"
        timeout  = "3s"
      }
    }
  }
}
