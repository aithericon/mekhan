# =============================================================================
# mekhan-service database — provisioned on the cluster's Patroni
# =============================================================================
# All credentials live in this layer's tfstate (encrypted in Hetzner Object
# Storage via the AES-GCM block in backend.tf). The Nomad jobspec reads the
# computed URL from local.database_url at apply time, so the password never
# has to round-trip through Vault or .envrc.
# =============================================================================

resource "random_password" "mekhan_dev" {
  length  = 24
  special = false      # no special chars → safe to embed in DSN without escaping
}

resource "postgresql_role" "mekhan_dev" {
  name                = "mekhan_dev"
  login               = true
  password            = random_password.mekhan_dev.result
  skip_reassign_owned = true
}

resource "postgresql_database" "mekhan_dev" {
  name              = "mekhan_dev"
  owner             = postgresql_role.mekhan_dev.name
  allow_connections = true
  template          = "template0"
  encoding          = "UTF8"
}

# Constructed once, consumed by main.tf when rendering the Nomad jobspec.
# Uses the *runtime* host (plain .service.consul) — Nomad allocations resolve
# that via the cluster's Consul DNS, regardless of operator-side DNS config.
locals {
  database_url = format(
    "postgres://%s:%s@%s:5432/%s",
    postgresql_role.mekhan_dev.name,
    random_password.mekhan_dev.result,
    var.postgres_runtime_host,
    postgresql_database.mekhan_dev.name,
  )
}
