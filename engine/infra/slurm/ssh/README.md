# Local Slurm test SSH keypair

`slurm_test` / `slurm_test.pub` is a **throwaway keypair committed on purpose**.

It exists only to reach the **locally-built, ephemeral Slurm test container**
defined in `../Dockerfile` + `../docker-compose.yml`. The public key is copied
into that container's `authorized_keys` at build time; the private key lets the
engine's test harness and the `slurm_dc` demo datacenter
(`demos/resources/slurm_dc.json`, `ssh_host: localhost`) SSH into the container
you spin up on your own machine.

It grants access to **nothing hosted, shared, or real**. Do not "rotate" it for
security reasons — it is a test fixture, not a secret. See the project
[`SECURITY.md`](../../../../SECURITY.md) "Known non-issues" section.
