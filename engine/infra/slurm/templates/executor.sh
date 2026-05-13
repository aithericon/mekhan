#!/bin/bash
#SBATCH --output=/tmp/petri-executor-%j.out
export EXECUTOR_NATS_URL=nats://host.docker.internal:4333
export EXECUTOR_NAMESPACE=executor_jobs
export EXECUTOR_SOURCE=nats_queue
export EXECUTOR_LIFETIME=run_to_completion
export EXECUTOR_MAX_JOBS=1
export EXECUTOR_CONCURRENCY=1
export EXECUTOR_NAME="slurm-executor-${SLURM_JOB_ID}"
export EXECUTOR_BASE_DIR=/tmp/aithericon-executor-slurm/${SLURM_JOB_ID}
export EXECUTOR_DEFAULT_TIMEOUT_SECS=60
export AITHERICON_SDK_PATH=/opt/petri/aithericon-sdk
export EXECUTOR_IDLE_TIMEOUT_SECS=300
echo "PETRI_TOKEN_DATA=$PETRI_TOKEN_DATA"
echo "Starting executor (SLURM job ${SLURM_JOB_ID})"
/opt/petri/bin/executor
