"""Seed a synthetic NAS tree on local disk so the crawl has real files to walk.

Self-contained: writes a deterministic ~7-file tree under /tmp/mekhan-crawl-demo
(cleaning any prior run first), the SAME path the `crawl` step's storage root
points at. No external file server, no credentials — the whole demo exercises
the file-ops crawl + inventory register path without touching a real NAS.
"""

import os
import shutil

from aithericon import log_info, set_output

ROOT = "/tmp/mekhan-crawl-demo"

# Deterministic content across a few subdirectories so the recursive walk has
# something to descend into.
FILES = {
    "datasets/genome_a.fasta": ">seqA\nACGTACGTACGT\n",
    "datasets/genome_b.fasta": ">seqB\nTTTTGGGGCCCC\n",
    "datasets/readme.txt": "synthetic dataset for the crawl demo\n",
    "plots/run_42.svg": "<svg><rect width='10' height='10'/></svg>\n",
    "plots/run_43.svg": "<svg><circle r='5'/></svg>\n",
    "logs/2026-06-07.log": "INFO seeded\nINFO ok\n",
    "logs/archive/old.log": "DEBUG legacy line\n",
}

shutil.rmtree(ROOT, ignore_errors=True)
for rel, content in FILES.items():
    path = os.path.join(ROOT, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)

n = len(FILES)
log_info(f"seeded {n} files under {ROOT}", root=ROOT, files_created=n)

set_output("root", ROOT)
set_output("files_created", n)
