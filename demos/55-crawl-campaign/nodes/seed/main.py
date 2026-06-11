"""Prepare the campaign target: seed the synthetic demo NAS, or pass an
override path through untouched.

The Start form's optional `path` field makes this demo point at ANY local
directory (an NFS mount, a workstation tree) instead of the synthetic NAS:

* `path` empty  -> seed a deterministic 63-file tree under
  /tmp/mekhan-campaign-demo (cleaning any prior run first) and emit
  root=/tmp, prefix=mekhan-campaign-demo/. 63 is deliberately not divisible
  by the demo chunk (batch_size 10 x max_batches 2), so the last iteration
  exercises the trailing-partial-batch path: 20 + 20 + 20 + 3.
* `path` set    -> seed NOTHING (files_created=0); split the path into
  root=<parent dir> + prefix=<basename>/ so the crawl walks the tree in
  place. The split (rather than prefix="") respects the crawl op's
  non-empty-prefix guard and gives adoption a sane endpoint root.

The crawl step borrows `{{ seed.root }}` / `{{ seed.prefix }}`, so this step
is the single place the demo-vs-override conditional lives — the placeholders
downstream stay dumb.
"""

import os
import shutil

from aithericon import log_info, set_output

# Literal slug read — DO NOT remove: the compiler scans for it and stages the
# Start field as this step's input.
override = start.path  # noqa: F821

override = (override or "").strip() if isinstance(override, str) else ""

if override:
    p = override.rstrip("/") or "/"
    root = os.path.dirname(p) or "/"
    prefix = os.path.basename(p) + "/"
    log_info(
        f"path override set — skipping seed; campaign walks {p} in place",
        root=root,
        prefix=prefix,
    )
    set_output("root", root)
    set_output("prefix", prefix)
    set_output("files_created", 0)
else:
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

    set_output("root", "/tmp")
    set_output("prefix", "mekhan-campaign-demo/")
    set_output("files_created", n)
