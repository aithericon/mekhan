#!/bin/sh
# Run the NetCDF-capable executor runner on agridos-nas in a glibc container.
#
# Reuses the EXISTING runner identity (creds under ~/.aithericon/executor/runner),
# so the engine pool lease + runner_id (bf3bf531-…) carry over unchanged — this
# is a drop-in replacement for the native ./bin/aithericon-executor.
#
# Requires docker access on the NAS (root / sudo, or the DSM Docker UI). The
# `admin` user cannot hit the docker socket directly on a stock DSM.
#
# BEFORE running this, STOP the native supervised runner so two processes don't
# share one runner identity on NATS:
#     kill "$(cat ~/aithericon-executor/run/supervise.pid)" 2>/dev/null || true
#     pkill aithericon-exec 2>/dev/null || true       # comm-match; NOT `pkill -f`
set -eu

IMG="aithericon-executor:netcdf"
ADMIN_HOME="/var/services/homes/admin"
DATA_ROOT="/var/services/homes/AgridosAPI/Data"   # crawl seed_root — mount at the SAME path

docker rm -f aithericon-executor 2>/dev/null || true

docker run -d --name aithericon-executor \
  --restart unless-stopped \
  --network host \
  -e AITHERICON_SDK_PATH=/opt/aithericon-sdk \
  -e RUST_LOG="info,aithericon_executor_worker=debug" \
  -v "${ADMIN_HOME}/.aithericon/executor:/root/.aithericon/executor:rw" \
  -v "${ADMIN_HOME}/aithericon-executor/aithericon-sdk:/opt/aithericon-sdk:ro" \
  -v "${DATA_ROOT}:${DATA_ROOT}:ro" \
  "${IMG}"

echo "started. follow logs with:  docker logs -f aithericon-executor"
