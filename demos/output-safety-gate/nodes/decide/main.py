# Aggregate verified flags into a {pass | warn | block} decision.
#
# Default policy (mirrors online-clinic's `safety_decision_from_flags`):
#   any verified flag with severity=critical    → block
#   else any verified flag with severity=warn   → warn
#   else                                        → pass
#
# Per-kind boost: contradicts_evidence is auto-critical regardless of the
# critic's self-assigned severity — direct contradictions are never "info".
#
# Upstream borrows:
#   verify.verified_flags   # [{kind, severity, message, supporting_text}]
#
# Outputs:
#   action ∈ {pass, warn, block}
#   reasons: list[str] of human-readable reason strings
#   critical_count, warn_count

define_phases(["Tally", "Decide"])

CRITICAL_KINDS = {"contradicts_evidence"}

update_phase("Tally", "running")

critical_count = 0
warn_count = 0
reasons: list[str] = []

for flag in (verify.verified_flags or []):
    kind = flag.get("kind", "unknown")
    sev = flag.get("severity", "info")
    msg = flag.get("message", "(no message)")

    if kind in CRITICAL_KINDS:
        sev = "critical"

    if sev == "critical":
        critical_count += 1
        reasons.append(f"[critical] {kind}: {msg}")
    elif sev == "warn":
        warn_count += 1
        reasons.append(f"[warn] {kind}: {msg}")
    else:
        reasons.append(f"[info] {kind}: {msg}")

log_metric("critical_flags", float(critical_count))
log_metric("warn_flags", float(warn_count))
update_phase("Tally", "completed")

update_phase("Decide", "running")
if critical_count > 0:
    action = "block"
elif warn_count > 0:
    action = "warn"
else:
    action = "pass"

set_output("action", action)
set_output("reasons", reasons)
set_output("critical_count", critical_count)
set_output("warn_count", warn_count)

if action == "block":
    log_warn("BLOCK", critical=critical_count, warn=warn_count)
elif action == "warn":
    log_warn("WARN", warn=warn_count)
else:
    log_info("PASS — no flags raised")

update_progress(1.0, action)
update_phase("Decide", "completed")
