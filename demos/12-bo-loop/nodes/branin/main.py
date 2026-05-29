"""Map body — the modified Branin-Hoo objective (minimization).

`cand` is the per-element itemVar the Map scatter stamps onto each body token
(`cand.a`, `cand.d` ride the token; no read-arc, no SDK init). The gathered
element is the value of the field named by the Map's `resultVar` (`obs`), so we
assign `obs` here. Lower z is better; the global minimum is ~0.3979.
"""

import math

a = cand.a
d = cand.d

# Map the unit square [0,1]^2 to the Branin domain.
x1 = 15.0 * a - 5.0
x2 = 15.0 * d

z = (
    (x2 - 5.1 / (4.0 * math.pi**2) * x1**2 + 5.0 / math.pi * x1 - 6.0) ** 2
    + 10.0 * (1.0 - 1.0 / (8.0 * math.pi)) * math.cos(x1)
    + 10.0
)

obs = {"a": a, "d": d, "z": z}
