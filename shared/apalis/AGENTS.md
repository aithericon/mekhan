# Repository Guidelines

## Project Structure & Module Organization
- Root crate: `src/lib.rs`; middleware layers in `src/layers/`.
- Workspace packages in `packages/`: `apalis-core`, `apalis-redis`, `apalis-sql`, `apalis-cron`, `apalis-nats`.
- Examples in `examples/` (Axum, Actix, Redis, SQL, Cron, Prometheus, etc.).
- Docs in `docs/` (e.g., `docs/nats.md`).

## Build, Test, and Development Commands
- Build workspace: `cargo build` (or `cargo build --release`).
- Build with features: `cargo build --features "limit,tracing,prometheus"`.
- Test all crates: `cargo test`; per crate: `cargo test -p apalis-sql`.
- Show test output: `cargo test -- --nocapture`.
- Run examples: `cargo run --example redis` (e.g., `REDIS_URL=redis://localhost cargo run --example redis`).

## Coding Style & Naming Conventions
- Rust 2021 edition; rustfmt defaults (4‑space indent). Enforce with:
  - Format: `cargo fmt` (CI uses `cargo fmt --check`).
  - Lint: `cargo clippy --all-features` and fix warnings where reasonable.
- Naming: modules/files snake_case; types/traits/enum variants PascalCase; functions/vars snake_case; constants SCREAMING_SNAKE_CASE.
- Public APIs must have `///` or crate docs `//!`; prefer explicit imports; keep examples minimal but runnable.

## Testing Guidelines
- Use `#[tokio::test]` for async tests. Co-locate unit tests in `mod tests {}`; integration tests live under `packages/*/tests/`.
- External deps: guard tests with env vars (e.g., `REDIS_URL`, `DATABASE_URL`) and skip when not set.
- Cover new behavior and edge cases; keep tests fast and deterministic. Run `cargo test` for the workspace before submitting.

## Commit & Pull Request Guidelines
- Use Conventional Commits style where possible: `feat:`, `fix:`, `docs:`, `chore:`, etc. Subject in imperative mood; details and rationale in body.
- PR checklist:
  - Pass `cargo fmt --check`, `cargo clippy --all-features`, and `cargo test`.
  - Describe changes, affected crates, and feature flags; link issues.
  - Update README/docs and CHANGELOG when user-facing.
  - Include or update an example when changing public behavior.

## Security & Configuration Tips
- Do not commit secrets. Configure via env vars: `REDIS_URL`, `DATABASE_URL`, `RUST_LOG`.
- Feature flags control optional integrations: `limit`, `tracing`, `prometheus`, `sentry`, `retry`, `timeout`, `filter`, `catch-panic`. Document new flags in `Cargo.toml` and README.

