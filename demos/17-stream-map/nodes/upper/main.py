# Map body — uppercase one streamed chunk (demo 16).
#
# `chunk` is the per-element itemVar the streaming Map stamps onto each body
# token. The chunk value rides on the inbound token, so read it as
# `input.chunk` (the runner exposes the token as `input`). NOTE: a SCALAR
# itemVar like this word string is reachable via `input.<itemVar>`, not as a
# bare `<itemVar>` global — the runner only auto-promotes *nested-object*
# itemVars (e.g. a `{a, d}` dict) to bare globals.
#
# `upper` is an implicit output: the runner sweeps any global matching a name
# declared in this step's output port (`upper`) at the end of execution. The
# Map lifts the body's `resultVar` (`upper`) value as the gathered element.

upper = str(input.chunk).upper()
