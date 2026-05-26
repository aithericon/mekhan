# =============================================================================
# Outputs — surfaced for CI verify-deploy + manual debugging
# =============================================================================

output "image_tag" {
  description = "Currently deployed mekhan-service image tag"
  value       = var.image_tag
}

output "job_id" {
  description = "Nomad job ID — feed into `nomad deployment status` for verify"
  value       = nomad_job.mekhan_service.id
}

output "job_namespace" {
  description = "Nomad namespace the job runs in"
  value       = var.nomad_namespace
}

output "database_url" {
  description = "Connection string mekhan-service uses at runtime. Sensitive — won't show in plan/apply output but `tofu output -raw database_url` exposes it for debugging."
  value       = local.database_url
  sensitive   = true
}

# ── Zitadel PAT-feature credentials ─────────────────────────────────────────
# Surfaced for debugging via `tofu output -raw <name>`. The same values are
# rendered into the Nomad jobspec via templatefile() in main.tf — these
# outputs are not the wiring path, just a way to read them back.

output "auth_introspection_client_id" {
  description = "client_id of the Mekhan introspection API app. Used as HTTP Basic username when Mekhan validates Bearer PATs via Zitadel /oauth/v2/introspect."
  value       = zitadel_application_api.introspect.client_id
  sensitive   = true
}

output "auth_introspection_client_secret" {
  description = "client_secret of the Mekhan introspection API app. Returned once at create-time, captured in tfstate; `tofu taint` to rotate."
  value       = zitadel_application_api.introspect.client_secret
  sensitive   = true
}

output "auth_broker_pat" {
  description = "PAT minted on the mekhan-token-broker machine user. Mekhan presents this to Zitadel Management API to broker per-user PATs. `tofu taint` zitadel_personal_access_token.token_broker to rotate."
  value       = zitadel_personal_access_token.token_broker.token
  sensitive   = true
}
