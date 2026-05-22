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
