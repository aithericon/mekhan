# Licensing

The Aithericon Platform is multi-licensed **per crate** by the strategic role
each component plays. The guiding principle: for a safe-AI / provenance
platform, the inspectability of the trust machinery *is* the product — so the
engine, SDKs, and standard surfaces are open source, while the deployable
control plane is source-available and protected against being resold as a
competing managed service.

## Per-crate licenses

| Crate / path | License | Role |
|---|---|---|
| `engine/core-engine` (+ `core-engine/crates/*`) | **Apache-2.0** | Colored Petri-net provenance/execution engine. The trust core. |
| `engine/sdk` (`aithericon-sdk`) | **Apache-2.0** | Scenario-definition DSL. Maximize adoption, zero friction. |
| `engine/sdk-derive` (`aithericon-sdk-derive`) | **Apache-2.0** | SDK proc macros. |
| `engine/cli` (`aithericon`) | **Apache-2.0** | CLI tooling. |
| `engine/simulator` (`petri-simulator`) | **Apache-2.0** | Scientific simulation. Permissive for academic adoption & citation. |
| `executor` (+ `executor/crates/*`) | **Apache-2.0** | Task/job executor. |
| `shared/secrets` (`aithericon-secrets`) | **Apache-2.0** | Secrets plumbing. |
| `shared/file-metadata` (`fmeta`) | **Apache-2.0** | Generic file-metadata utility. |
| `shared/apalis` (`apalis`, `apalis-nats`) | **MIT OR Apache-2.0** | Vendored fork of upstream `apalis`; license dictated by upstream. See `NOTICE`. |
| `service` (`mekhan-service`) | **FSL-1.1-ALv2** | Deployable control plane / orchestrator. Source-available; converts to Apache-2.0 two years after each release. |

Enterprise features and the managed cloud are **not** in this repository. They
are separate proprietary components under a commercial agreement, loaded by the
open core through stable interfaces. They are never licensed under FSL or
Apache-2.0.

## What FSL-1.1-ALv2 means for `service`

- **You may** use, modify, self-host, and run it in production — including
  commercially and internally — for any purpose.
- **You may not** put it to a *Competing Use*: making it available to others
  in a commercial product or service that substitutes for the Software, for
  another product/service we offer using it, or that offers the same or
  substantially similar functionality. Offering it as a managed/hosted service
  is the central prohibited case.
- **Eventual open source:** each released version of `service` is irrevocably
  additionally licensed under **Apache-2.0**, effective **two years** after
  that version is made available.

Full texts: [`LICENSE-APACHE`](./LICENSE-APACHE), [`LICENSE-FSL`](./LICENSE-FSL).

## Language

Describe the platform precisely: **"open-source engine & SDK (Apache-2.0),
source-available control plane (FSL, converts to Apache-2.0 in 2 years),
commercial cloud."** FSL is *source-available*, not OSI "open source" — do not
conflate the two in public materials.

## Trademarks

Trademarks are reserved regardless of code license, including over the
Apache-2.0 crates. Code may be forked under its license; the project and
product names may not be used except to identify origin.

## Contributing

Contributions are accepted under the inbound=outbound license of the crate
being modified, with a Developer Certificate of Origin sign-off. See
[`CONTRIBUTING.md`](./CONTRIBUTING.md).
