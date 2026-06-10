"""Seed a synthetic NAS tree on local disk so the campaign has files to walk.

Self-contained: writes a deterministic 63-file tree under
/tmp/mekhan-campaign-demo (cleaning any prior run first), the SAME path the
crawl step's storage root points at. 63 is deliberately not divisible by the
campaign's 20-files-per-chunk (batch_size 10 × max_batches 2), so the last
iteration exercises the trailing-partial-batch path: 20 + 20 + 20 + 3.
"""

import os
import shutil

from aithericon import log_info, set_output

ROOT = "/tmp/mekhan-campaign-demo"

shutil.rmtree(ROOT, ignore_errors=True)

n = 0
for shard in range(7):  # 7 shards × 9 files = 63
    for i in range(9):
        rel = f"shard_{shard:02d}/file_{i:03d}.txt"
        path = os.path.join(ROOT, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(f"campaign demo content shard={shard} i={i}\n")
        n += 1

log_info(f"seeded {n} files under {ROOT}", root=ROOT, files_created=n)

set_output("root", ROOT)
set_output("files_created", n)
