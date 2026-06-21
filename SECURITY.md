# Security Policy

## Project maturity

The Aithericon Platform is **early alpha** software shared for open development.
It is **not production-hardened**. In particular, the local development stack
(`just dev`, `docker compose`) ships intentionally insecure defaults so the
platform can run offline in a single command:

- a dev Vault running with root token `root` and no persistence,
- a no-op auth mode (`dev_noop`) where every request is a fixed admin user,
- default object-store credentials (`rustfsadmin` / `rustfsadmin`).

**Never expose a dev-default deployment to an untrusted network, and never put
real or sensitive data into one.** Production-grade auth, secret management,
TLS, and tenancy isolation are under active development.

## Reporting a vulnerability

Please report suspected security vulnerabilities **privately**:

- Email: **security@aithericon.com**
- Or use GitHub's "Report a vulnerability" (Security → Advisories) on this repo.

Please do **not** open public issues or pull requests for security problems.

We'll acknowledge your report, work with you on a fix, and credit you (if you
wish) once a fix is available. As an alpha project we can't promise a fixed SLA
yet, but we take reports seriously and will respond as quickly as we can.

## Known non-issues (please don't report these)

Automated scanners flag the following. They are **intentional, self-contained
test fixtures** that grant access to nothing real:

- `engine/infra/slurm/ssh/slurm_test{,.pub}` — a throwaway SSH keypair used only
  to reach a **locally-built, ephemeral Slurm test container** (`ssh_host:
  localhost`). The public key is baked into that container's `authorized_keys`
  at build time; the private key lets the test harness SSH into your own local
  container. It does not unlock any hosted or shared infrastructure.
- `AKIAIOSFODNN7EXAMPLE` and similar — the well-known AWS documentation example
  credentials, used in tests. Not real keys.
- Dummy `-----BEGIN ... PRIVATE KEY-----` / NATS JWT / NKEY strings in test
  files — fixtures, not live credentials.
