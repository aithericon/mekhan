#!/bin/bash
#SBATCH -J puck-firing
#SBATCH -p epyc-256
#SBATCH -N 1
#SBATCH -n 1
#SBATCH --mem=4G
#SBATCH -t 00:30:00
#SBATCH -o /home/ender/aithericon/jobs/puck-%j.out

# Campaign parameters (override via: sbatch --export=ALL,RAMP_RATE=10,... )
RAMP_RATE=${RAMP_RATE:-5}       # K/min
HOLD_TEMP=${HOLD_TEMP:-1850}    # K
HOLD_TIME=${HOLD_TIME:-3600}    # s
COOL_RATE=${COOL_RATE:-10}      # K/min

source /etc/profile
module load openmpi/4.1
source /software/openfoam/v2506/OpenFOAM-v2506/etc/bashrc

TEMPLATE=/home/ender/aithericon/cases/puck-firing
RUN_DIR=/home/ender/aithericon/runs/puck_${SLURM_JOB_ID}
mkdir -p /home/ender/aithericon/runs
cp -r "$TEMPLATE" "$RUN_DIR"
cd "$RUN_DIR"
rm -rf 0 postProcessing log.* VTK results.json

# Inject campaign parameters (the only templated file)
cat > system/caseParams << EOF
FoamFile
{
    version     2.0;
    format      ascii;
    class       dictionary;
    object      caseParams;
}

rampRate        $RAMP_RATE;
holdTemp        $HOLD_TEMP;
holdTime        $HOLD_TIME;
coolRate        $COOL_RATE;

T0              300;
nSteps          800;
nWrites         20;

tRampEnd        #eval{ (\$holdTemp - \$T0) / (\$rampRate / 60.0) };
tHoldEnd        #eval{ \$tRampEnd + \$holdTime };
tEnd            #eval{ \$tHoldEnd + (\$holdTemp - \$T0) / (\$coolRate / 60.0) };
dt              #eval{ \$tEnd / \$nSteps };
writeInt        #eval{ \$tEnd / \$nWrites };
EOF

cp -r 0.orig 0
blockMesh > log.blockMesh 2>&1 || { echo "BLOCKMESH_FAILED"; exit 1; }
solidDisplacementFoam > log.solidDisplacementFoam 2>&1 || { echo "SOLVER_FAILED"; tail -30 log.solidDisplacementFoam; exit 1; }
foamToVTK -fields '(T sigmaEq)' > log.foamToVTK 2>&1 || echo "VTK_FAILED (non-fatal)"

# ---- extract objectives -> results.json ----
export RAMP_RATE HOLD_TEMP HOLD_TIME COOL_RATE
python3 /home/ender/aithericon/jobs/extract-results.py "$RUN_DIR"
echo "PUCK_RESULT=$?"
