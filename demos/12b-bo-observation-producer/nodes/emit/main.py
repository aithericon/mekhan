"""BO observation producer — the catalogue-side half of Phase 4.

Writes a single observation artifact and logs it through the executor's IPC
sidecar via ``aithericon.log_artifact``. The catalogue_register effect (engine
side) turns the LogArtifact event into a CatalogueRegisterCommand, the mekhan
causality ingest projects it into a catalogue entry, and the 12a `BO Catalog
Trigger` (filtering category=metric AND user_metadata.kind=bo_observation)
fires -> spawns a slim re-fit instance.

WHY category='metric' and not 'bo_observation': ArtifactCategory is a CLOSED
proto enum (model/dataset/plot/log/checkpoint/config/metric/other). Any string
the SDK doesn't recognise falls back to OTHER. The BO semantics therefore ride
in a `kind` user_metadata sentinel, which the catalog trigger filters on via
`user_metadata.kind`.

WHY everything in metadata is a string: artifact metadata is a proto
map<string,string>; the engine keeps only string-valued metadata into the
catalogue entry's user_metadata. So the observation list is json.dumps'd and z
is str()'d. The 12a Start fields are authored kind:json precisely so these
string payloads pass the strict Start-contract gate; 12a's body json.loads them.

IMPORTANT (compiler contract): the literal reads of `start.a`, `start.d`,
`start.z` below are SCANNED at compile time to synthesize read-arcs. Keep them
verbatim.
"""

import json

import aithericon

# --- Read the borrowed seed point (literal slug reads — DO NOT remove) -------
a = start.a
d = start.d
z = start.z

# Defaults so the demo fires even with an empty Start token.
a = float(a) if a is not None else 0.3
d = float(d) if d is not None else 0.7
z = float(z) if z is not None else 1.42

observations = [{"a": a, "d": d, "z": z}]

with open("obs.json", "w") as f:
    json.dump({"observations": observations, "z": z}, f)

log_info(f"producer: logging observation a={a} d={d} z={z}")

# Every metadata VALUE must be a string (proto map<string,string>).
aithericon.log_artifact(
    "obs.json",
    name="bo_obs",
    category="metric",
    metadata={
        "kind": "bo_observation",
        "observations": json.dumps(observations),
        "z": str(z),
    },
    blocking=True,
)

logged = True
log_info("producer: observation artifact logged")
