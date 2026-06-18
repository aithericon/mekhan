# Consumers

Durable consumers and their filter subjects. Live-verified against slot-0
(2026-06-18). All `PETRI_GLOBAL` consumers use `petri.*.*.…` wildcards on the two
tenant tokens.

# By role

* [Mekhan projections](mekhan-projections.md) - the `mekhan-*-v2` read-model fleet plus ingest consumers.
* [Engine listeners](engine-listeners.md) - signal / bridge / create-net listeners and per-net ephemeral consumers.
* [Result ingest](result-ingest.md) - human-task and executor result drains.
