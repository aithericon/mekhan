# =============================================================================
# mailpit Nomad job — cluster SMTP capture server (deploy-time mirror of
# `just dev mailhog-up`). Lets the `mail` SMTP resource resolve in-cluster
# (mailhog.service.consul:1025) instead of an unreachable localhost.
# Rendering pattern mirrors nomad_job "executor" in executor.tf.
# =============================================================================

variable "mailpit_image" {
  description = "Mailpit container image (multi-arch — works on the arm64 nodes)."
  type        = string
  default     = "axllent/mailpit:latest"
}

variable "mailpit_smtp_port" {
  description = "Static host port for the SMTP receiver the `mail` resource targets."
  type        = number
  default     = 1025
}

variable "mailpit_cpu_mhz" {
  type    = number
  default = 100
}

variable "mailpit_memory_mb" {
  type    = number
  default = 128
}

resource "nomad_job" "mailpit" {
  jobspec = templatefile("${path.module}/mailpit.nomad.hcl.tpl", {
    namespace   = var.nomad_namespace
    datacenters = jsonencode(var.nomad_datacenters)
    node_class  = var.node_class
    hostname    = var.hostname
    image       = var.mailpit_image
    smtp_port   = var.mailpit_smtp_port
    cpu_mhz     = var.mailpit_cpu_mhz
    memory_mb   = var.mailpit_memory_mb
  })

  purge_on_destroy = true
}
