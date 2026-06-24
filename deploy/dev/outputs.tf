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

# ── Mekhan Testers org ───────────────────────────────────────────────────────

output "mekhan_testers_org_id" {
  description = "Numeric ID of the dedicated 'Mekhan Testers' Zitadel org. Bind a Mekhan workspace to this (workspaces.zitadel_org_id) to land testers in an isolated workspace."
  value       = zitadel_org.mekhan_testers.id
}

# ── Testers workspace one-time bootstrap ─────────────────────────────────────
# Mekhan derives a user's internal id as uuidv5(NAMESPACE, zitadel_subject),
# where NAMESPACE is the constant SUBJECT_UUID_NAMESPACE in
# service/src/auth/model.rs (0x6d65...7635). The Zitadel `sub` equals the user
# resource id, so we can compute the exact workspace_members.user_id here.
locals {
  mekhan_subject_uuid_namespace = "6d656b68-616e-5f73-756a-6563745f7635"

  workspace_owner_user_id = var.workspace_owner_user_key == "" ? null : uuidv5(
    local.mekhan_subject_uuid_namespace,
    zitadel_human_user.tester[var.workspace_owner_user_key].id,
  )
}

output "workspace_owner_user_id" {
  description = "Mekhan workspace_members.user_id (uuidv5 of the Zitadel subject) for the designated workspace owner. null when workspace_owner_user_key is unset."
  value       = local.workspace_owner_user_id
}

output "testers_workspace_bootstrap_sql" {
  description = "ONE-TIME bootstrap SQL. Run once against the mekhan_dev DB after the first deploy to create the 'Testers' workspace bound to the Mekhan Testers org and seed the owner. Idempotent. After this, all onboarding is in-app. Read with: tofu output -raw testers_workspace_bootstrap_sql"
  value       = var.workspace_owner_user_key == "" ? null : <<-SQL
    WITH ws AS (
      INSERT INTO workspaces (slug, display_name, zitadel_org_id)
      VALUES ('testers', 'Testers', '${zitadel_org.mekhan_testers.id}')
      ON CONFLICT (slug) DO UPDATE SET zitadel_org_id = EXCLUDED.zitadel_org_id
      RETURNING id
    )
    INSERT INTO workspace_members (workspace_id, user_id, role)
    SELECT id, '${local.workspace_owner_user_id}'::uuid, 'owner' FROM ws
    ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = 'owner';
  SQL
}
