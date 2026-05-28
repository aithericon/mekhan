#!/usr/bin/env bash
#
# Seed a Scheduled-Slurm template + instance through the LIVE mekhan-service
# (default :3100), so the UI at :5173 has a real workflow to inspect.
#
# Mirrors the graph in service/tests/scheduled_slurm_e2e.rs but lands the row
# in the live `mekhan` Postgres (the test uses an ephemeral fixture DB the UI
# can't see).
#
# Prereqs:
#   just dev up          # postgres + nats + rustfs + engine + mekhan + app
#   just dev slurm-up    # Docker Slurm cluster + engine in SCHEDULER_BACKEND=slurm
#
# Usage:
#   just/scripts/seed-slurm-instance.sh                # POSTs to localhost:3100
#   MEKHAN_URL=http://other:3100 just/scripts/seed-slurm-instance.sh
#   JOB_TEMPLATE=mekhan-executor-worker  just/scripts/seed-slurm-instance.sh
#
# On success, prints the template + instance UUIDs and the UI URLs.

set -euo pipefail

MEKHAN_URL="${MEKHAN_URL:-http://localhost:3100}"
JOB_TEMPLATE="${JOB_TEMPLATE:-mekhan-executor-worker}"
TEMPLATE_NAME="${TEMPLATE_NAME:-Scheduled-Slurm UI Seed}"

# Tiny aithericon-SDK-using Python step (same script the e2e test uses).
# Persisted on the AutomatedStep node as `main.py`; staged to S3 at publish;
# pulled by the Slurm-launched executor at run time.
read -r -d '' MAIN_PY <<'PY' || true
from _aithericon_io import load_input

load_input()
log_info("scheduled-slurm UI seed ran")
set_output("ran", True)
set_output("answer", 42)
PY

# Build the CreateTemplateRequest body. Keeping the graph in Python (vs
# heredoc'ing JSON) so the inline main.py round-trips through `json.dumps`
# and we don't have to escape quotes. The shape matches the OpenAPI
# `CreateTemplateRequest` schema; field names follow `serde(rename_all =
# camelCase)` on the Rust DTOs.
template_body=$(
  MAIN_PY="$MAIN_PY" \
  TEMPLATE_NAME="$TEMPLATE_NAME" \
  JOB_TEMPLATE="$JOB_TEMPLATE" \
  python3 - <<'PY'
import json, os

empty_port = lambda pid, label: {"id": pid, "label": label, "fields": []}

graph = {
    "nodes": [
        {
            "id": "s",
            "type": "start",
            "position": {"x": 0, "y": 0},
            "data": {
                "type": "start",
                "label": "Start",
                "initial": empty_port("in", "Input"),
            },
        },
        {
            "id": "auto",
            "type": "automated_step",
            "position": {"x": 240, "y": 0},
            "data": {
                "type": "automated_step",
                "label": "Run Python (Scheduled Slurm)",
                "executionSpec": {
                    "backendType": "python",
                    "entrypoint": "main.py",
                    "config": {
                        "python": "python3",
                        "requirements": [],
                        "virtualenv": False,
                        "sdk": True,
                        "inherit_env": True,
                        "env": {},
                    },
                },
                "input":  empty_port("in",  "Input"),
                "output": empty_port("out", "Output"),
                "deploymentModel": {
                    "mode": "scheduled",
                    "jobTemplate": os.environ["JOB_TEMPLATE"],
                },
            },
        },
        {
            "id": "e",
            "type": "end",
            "position": {"x": 480, "y": 0},
            "data": {
                "type": "end",
                "label": "End",
                "terminal": empty_port("in", "Terminal"),
                "resultMapping": [],
            },
        },
    ],
    "edges": [
        {"id": "e1", "source": "s",    "target": "auto", "targetHandle": "in", "type": "sequence"},
        {"id": "e2", "source": "auto", "target": "e",    "targetHandle": "in", "type": "sequence"},
    ],
}

body = {
    "name": os.environ["TEMPLATE_NAME"],
    "description": (
        "Authored via just/scripts/seed-slurm-instance.sh — UI-visible "
        "counterpart of service/tests/scheduled_slurm_e2e.rs."
    ),
    "graph": graph,
    "files": {"auto": {"main.py": os.environ["MAIN_PY"]}},
}
print(json.dumps(body))
PY
)

echo "▶ POST $MEKHAN_URL/api/v1/templates …"
template_resp=$(curl -sf -X POST "$MEKHAN_URL/api/v1/templates" \
    -H 'content-type: application/json' \
    -d "$template_body")
template_id=$(echo "$template_resp" | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
echo "  ✓ template $template_id"

echo "▶ POST $MEKHAN_URL/api/v1/templates/$template_id/publish …"
publish_resp=$(curl -sf -X POST "$MEKHAN_URL/api/v1/templates/$template_id/publish")
version=$(echo "$publish_resp" | python3 -c "import json,sys; print(json.load(sys.stdin).get('version','?'))")
echo "  ✓ published v$version"

echo "▶ POST $MEKHAN_URL/api/v1/instances …"
instance_body=$(python3 -c "import json; print(json.dumps({'template_id': '$template_id', 'metadata': {'source': 'seed-slurm-instance.sh'}}))")
instance_resp=$(curl -sf -X POST "$MEKHAN_URL/api/v1/instances" \
    -H 'content-type: application/json' \
    -d "$instance_body")
instance_id=$(echo "$instance_resp" | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
status=$(echo "$instance_resp" | python3 -c "import json,sys; print(json.load(sys.stdin)['status'])")
echo "  ✓ instance $instance_id ($status)"

cat <<EOF

Open in UI:
  http://localhost:5173/instances/$instance_id
  http://localhost:5173/templates/$template_id

Poll status:
  curl -sf $MEKHAN_URL/api/v1/instances/$instance_id | python3 -m json.tool
EOF
