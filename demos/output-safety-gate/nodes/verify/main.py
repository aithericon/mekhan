# Verify that each critic flag's `supporting_text` actually appears in the
# subject text. Drops flags whose quote can't be found — these are fabricated
# quotes, and we'd rather miss a real issue than block on a hallucinated one.
#
# Mekhan analogue of the clinic's `provenance` step kind
# (server/src/services/pipeline/citation.rs:hybrid_auto matcher,
# min_confidence=0.7).
#
# Upstream borrows:
#   start.subject_text   # what the critic was looking at
#   critic.flags         # [{kind, severity, message, supporting_text}]
#
# Outputs:
#   verified_flags, dropped_flags, verified_count, dropped_count

import re

define_phases(["Normalize subject", "Match flags"])

def normalize(s: str) -> str:
    return re.sub(r"\s+", " ", (s or "").lower()).strip()

update_phase("Normalize subject", "running")
hay = normalize(start.subject_text)
log_info("subject normalized", chars=len(hay))
update_phase("Normalize subject", "completed")

update_phase("Match flags", "running")

verified_flags: list = []
dropped_flags: list = []

for flag in (critic.flags or []):
    quote = normalize(flag.get("supporting_text", ""))
    if not quote:
        dropped_flags.append({**flag, "drop_reason": "empty_supporting_text"})
        log_debug("dropped: empty supporting_text", kind=flag.get("kind"))
        continue
    if quote in hay:
        verified_flags.append(flag)
    else:
        dropped_flags.append({**flag, "drop_reason": "quote_not_in_subject"})
        log_debug(
            "dropped: quote not in subject",
            kind=flag.get("kind"),
            excerpt=flag.get("supporting_text", "")[:80],
        )

log_metric("flags_verified", float(len(verified_flags)))
log_metric("flags_dropped", float(len(dropped_flags)))

set_output("verified_flags", verified_flags)
set_output("dropped_flags", dropped_flags)
set_output("verified_count", len(verified_flags))
set_output("dropped_count", len(dropped_flags))

update_progress(1.0, f"{len(verified_flags)} kept / {len(dropped_flags)} dropped")
update_phase("Match flags", "completed")
