# Map body — uppercase one streamed chunk (demo 16).
#
# `chunk` is the per-element itemVar the streaming Map stamps onto each body
# token (the chunk value rides on the token; no read-arc, no SDK init — the
# runner promotes it to a global). Here each chunk value is a plain word string.
#
# `upper` is an implicit output: the runner sweeps any global matching a name
# declared in this step's output port (`upper`) at the end of execution. The
# Map lifts the body's `resultVar` (`upper`) value as the gathered element.

upper = str(chunk).upper()
