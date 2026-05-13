#!/bin/bash
#SBATCH --output=/tmp/petri-job-%j.out
echo "PETRI_TOKEN_DATA=$PETRI_TOKEN_DATA"
sleep 2
echo "Job complete"
