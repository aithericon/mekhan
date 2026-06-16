"""Case generation for the firing-curve evaluator (composite library node f001).

Builds the complete solidDisplacementFoam case (zirconia 3Y-TZP puck,
Ø60 x 12 mm, quarter symmetry, kiln-curve table BC) for the inbound firing curve
and emits it as a `case_files` bundle for the generic `openfoam/run-case` solver
node — plus the solver flags and the timing scalars the downstream `extract`
node needs (the soak window + the cycle time). The firing-specific GEOMETRY /
MATERIAL / BC live HERE; the solve is delegated to the generic node; metric
extraction + scoring live in `extract`.

`solver_mode == "surrogate"` sets `dry_run`, so the generic node skips Docker
entirely and `extract` evaluates the calibrated closed-form surrogate instead.
"""

# --- Firing curve (Start fields; optionals fall back to calibrated defaults) --
ramp_rate = float(input.ramp_rate)               # K/min  (required)
cool_rate = float(input.cool_rate)               # K/min  (required)
hold_time_s = float(input.hold_time_s)           # s      (required)
hold_temp = float(input.hold_temp or 1850.0)     # K
sigma_limit = float(input.sigma_limit or 80.0)   # MPa
solver_mode = str(input.solver_mode or "auto")

T0 = 300.0
N_STEPS = 800

t_ramp_end = (hold_temp - T0) / (ramp_rate / 60.0)
t_hold_end = t_ramp_end + hold_time_s
t_end = t_hold_end + (hold_temp - T0) / (cool_rate / 60.0)
cycle_h = t_end / 3600.0

log_info(
    f"casegen: ramp={ramp_rate} K/min hold={hold_time_s:.0f}s@{hold_temp:.0f}K "
    f"cool={cool_rate} K/min -> cycle {cycle_h:.2f} h (mode={solver_mode})"
)


# --- Case generation ----------------------------------------------------------
# The full validated case, embedded. Only `system/caseParams` carries the
# candidate's parameters — every other file is byte-stable across runs.

def _foam_header(cls, obj):
    return (
        "FoamFile\n{\n"
        "    version     2.0;\n"
        "    format      ascii;\n"
        f"    class       {cls};\n"
        f"    object      {obj};\n"
        "}\n"
    )


CASE_FILES = {
    "system/caseParams": _foam_header("dictionary", "caseParams")
    + f"""
rampRate        {ramp_rate};
holdTemp        {hold_temp};
holdTime        {hold_time_s};
coolRate        {cool_rate};

T0              {T0};
nSteps          {N_STEPS};
nWrites         20;

tRampEnd        #eval{{ ($holdTemp - $T0) / ($rampRate / 60.0) }};
tHoldEnd        #eval{{ $tRampEnd + $holdTime }};
tEnd            #eval{{ $tHoldEnd + ($holdTemp - $T0) / ($coolRate / 60.0) }};
dt              #eval{{ $tEnd / $nSteps }};
writeInt        #eval{{ $tEnd / $nWrites }};
""",
    "system/blockMeshDict": _foam_header("dictionary", "blockMeshDict")
    + """
// Quarter cylinder (puck): R=30mm, H=12mm (scale 1.5 on base R=20mm coords).
scale   1.5;

vertices
(
    (0          0          0)
    (0.008      0          0)
    (0.02       0          0)
    (0.0141421  0.0141421  0)
    (0.008      0.008      0)
    (0          0.008      0)
    (0          0.02       0)
    (0          0          0.008)
    (0.008      0          0.008)
    (0.02       0          0.008)
    (0.0141421  0.0141421  0.008)
    (0.008      0.008      0.008)
    (0          0.008      0.008)
    (0          0.02       0.008)
);

blocks
(
    hex (0 1 4 5  7 8 11 12)   (8 8 10) simpleGrading (1 1 1)
    hex (1 2 3 4  8 9 10 11)   (8 8 10) simpleGrading (1 1 1)
    hex (5 4 3 6  12 11 10 13) (8 8 10) simpleGrading (1 1 1)
);

edges
(
    arc  2  3 (0.0184776 0.0076537 0)
    arc  9 10 (0.0184776 0.0076537 0.008)
    arc  3  6 (0.0076537 0.0184776 0)
    arc 10 13 (0.0076537 0.0184776 0.008)
);

boundary
(
    symX
    {
        type symmetryPlane;
        faces ((0 1 8 7) (1 2 9 8));
    }
    symY
    {
        type symmetryPlane;
        faces ((0 7 12 5) (5 12 13 6));
    }
    outer
    {
        type patch;
        faces ((2 3 10 9) (3 6 13 10));
    }
    bottom
    {
        type patch;
        faces ((0 5 4 1) (1 4 3 2) (5 6 3 4));
    }
    top
    {
        type patch;
        faces ((7 8 11 12) (8 9 10 11) (12 11 10 13));
    }
);
""",
    "system/controlDict": _foam_header("dictionary", "controlDict")
    + """
#include "caseParams"

application     solidDisplacementFoam;
startFrom       startTime;
startTime       0;
stopAt          endTime;
endTime         $tEnd;
deltaT          $dt;
writeControl    runTime;
writeInterval   $writeInt;
purgeWrite      0;
writeFormat     ascii;
writePrecision  6;
writeCompression off;
timeFormat      general;
timePrecision   6;
runTimeModifiable true;

functions
{
    minMaxT
    {
        type            fieldMinMax;
        libs            (fieldFunctionObjects);
        fields          (T);
        writeControl    timeStep;
        writeInterval   1;
        log             false;
    }
}
""",
    "system/fvSchemes": _foam_header("dictionary", "fvSchemes")
    + """
// Quasi-static stress (steadyState d2dt2) + transient heat conduction.
d2dt2Schemes  { default steadyState; }
ddtSchemes    { default Euler; }
gradSchemes
{
    default         leastSquares;
    grad(D)         leastSquares;
    grad(T)         leastSquares;
}
divSchemes
{
    default         none;
    div(sigmaD)     Gauss linear;
}
laplacianSchemes
{
    default         none;
    laplacian(DD,D) Gauss linear corrected;
    laplacian(DT,T) Gauss linear corrected;
}
interpolationSchemes { default linear; }
snGradSchemes        { default none; }
""",
    "system/fvSolution": _foam_header("dictionary", "fvSolution")
    + """
solvers
{
    "(D|T)"
    {
        solver          GAMG;
        tolerance       1e-06;
        relTol          0.9;
        smoother        GaussSeidel;
        nCellsInCoarsestLevel 20;
    }
}

stressAnalysis
{
    compactNormalStress yes;
    nCorrectors     1;
    D               1e-06;
}
""",
    "constant/mechanicalProperties": _foam_header("dictionary", "mechanicalProperties")
    + """
// Zirconia (3Y-TZP).
rho { type uniform; value 6050; }
nu  { type uniform; value 0.30; }
E   { type uniform; value 2.0e+11; }
planeStress     no;
""",
    "constant/thermalProperties": _foam_header("dictionary", "thermalProperties")
    + """
// Zirconia (3Y-TZP); thermal stress coupling ON.
C     { type uniform; value 460; }
k     { type uniform; value 2.0; }
alpha { type uniform; value 1.0e-05; }
thermalStress   yes;
""",
    "0/T": _foam_header("volScalarField", "T")
    + """
#include "../system/caseParams"

dimensions      [0 0 0 1 0 0 0];
internalField   uniform $T0;

boundaryField
{
    symX { type symmetryPlane; }
    symY { type symmetryPlane; }
    "(outer|top|bottom)"
    {
        type            uniformFixedValue;
        uniformValue    table
        (
            (0          $T0)
            ($tRampEnd  $holdTemp)
            ($tHoldEnd  $holdTemp)
            ($tEnd      $T0)
        );
    }
}
""",
    "0/D": _foam_header("volVectorField", "D")
    + """
dimensions      [0 1 0 0 0 0 0];
internalField   uniform (0 0 0);

boundaryField
{
    symX { type symmetryPlane; }
    symY { type symmetryPlane; }
    "(outer|top|bottom)"
    {
        type            tractionDisplacement;
        traction        uniform (0 0 0);
        pressure        uniform 0;
        value           uniform (0 0 0);
    }
}
""",
}


# --- Outputs (swept from globals matching the output port) ---------------------
# The whole case as a portable bundle for the generic `openfoam/run-case` node,
# the solver flags, and the firing params + timing scalars `extract` needs. The
# firing scalars (ramp_rate/.../t_end/T0) are already module globals above.
case_files = CASE_FILES
solver = "solidDisplacementFoam"
run_blockmesh = True
export_vtk = True
# Surrogate mode skips the Docker solve entirely (extract owns the closed-form).
dry_run = solver_mode == "surrogate"
