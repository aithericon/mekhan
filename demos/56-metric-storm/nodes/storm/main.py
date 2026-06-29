"""Metric Storm — memory-profiling load generator.

Emits a large number of streaming metric + log events with NO artifacts and NO
marking tokens, to reproduce the crawl's metric firehose in isolation. Sinks
correctly keep telemetry out of the marking/event log (firing.rs PlaceKind::Sink
drops the token before any TokenCreated is emitted), so this load stresses the
engine's *ingest/consumer/relay* path under high streaming-event volume — the
remaining suspect for the 2026-06-29 engine OOM.

Tunable via the MEKHAN_METRIC_STORM_COUNT env (default 100_000); the env is
inherited (inherit_env: true).
"""

import os

import aithericon

COUNT = int(os.environ.get("MEKHAN_METRIC_STORM_COUNT", "100000"))

for i in range(COUNT):
    aithericon.log_metric(
        "crawl.batch.files",
        400.0,
        step=i,
        labels={
            "batch": str(i),
            "probe": "full",
            "endpoint_root": "/var/services/homes/AgridosAPI",
            "prefix": "Data/",
            "last_path": f"Data/nodes/885a2225-fa9d-404d-aea8/fft_time_f2,8GHz_{i}.png",
            "runner": "metric-storm",
        },
    )
    if i % 1000 == 0:
        aithericon.log_info(f"metric storm progress: {i}/{COUNT}")

aithericon.set_output("emitted", COUNT)
