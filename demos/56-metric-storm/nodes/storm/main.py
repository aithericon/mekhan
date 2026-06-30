"""Metric Storm — paced streaming-metric load generator.

Emits streaming metric + log events at a PACED rate so the engine's
`sig_metric -> metric_log` (sink) drain transition can keep up, the way a real
crawl does (~handful of metrics per batch). Without pacing, an unpaced tight
loop overruns the consumer: metrics pile undrained at the `sig_metric` signal
place and the ExecutorWatcher's telemetry consumer buffers the firehose in RAM
— which is exactly the OOM this fixture exists to exercise, so it must be PACED
to be a useful (non-self-DoSing) load test.

Tunable via env (inherit_env: true):
  MEKHAN_METRIC_STORM_COUNT     number of metrics to emit (default 5000)
  MEKHAN_METRIC_STORM_DELAY_MS  ms to sleep between metrics (default 3)
"""

import os
import time

import aithericon

COUNT = int(os.environ.get("MEKHAN_METRIC_STORM_COUNT", "5000"))
DELAY_S = float(os.environ.get("MEKHAN_METRIC_STORM_DELAY_MS", "3")) / 1000.0

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
    if DELAY_S > 0:
        time.sleep(DELAY_S)

aithericon.set_output("emitted", COUNT)
