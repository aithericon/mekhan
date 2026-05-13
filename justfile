# Mekhan command runner — entry point for local dev + CI.
# Run `just` for a recipe list, `just <module>::<recipe>` to execute.

set dotenv-load := true
set shell := ["bash", "-c"]
set export := true

# ── Registry / build defaults (overridable via env) ──────────────────────────
docker_registry  := env_var_or_default("DOCKER_REGISTRY", "registry.aithericon.eu")
docker_namespace := env_var_or_default("DOCKER_NAMESPACE", "mekhan")

# Cross-compilation target — matches production Nomad cluster (ARM64 musl)
target_triple    := env_var_or_default("TARGET_TRIPLE", "aarch64-unknown-linux-musl")

# ── Module imports ───────────────────────────────────────────────────────────
mod ci 'just/ci.just'
mod dev 'just/dev.just'
mod db 'just/db.just'

# Default recipe: show available recipes
default:
    @just --list
