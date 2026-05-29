#!/usr/bin/env bash
# Robustly launch a long-lived dev daemon, fully detached, with early-crash
# detection so a fast death surfaces the captured log instead of a phantom ✓.
#
#   spawn-daemon.sh <name> <pidfile> <logfile> -- <cmd> [args...]
#
# The caller is expected to build <cmd> as an `env VAR=val ... <binary> [args]`
# invocation (mirrors the old `nohup env ...` idiom). All child env must be in
# <cmd>; this script adds none.
#
# Exit status: 0 and prints the child PID on stdout if the daemon is still
# alive after the early-crash window; 1 (with the log tail on stderr) if it
# died immediately. Because the dev recipes run under `set -euo pipefail`, a
# non-zero exit here aborts `just dev up` loudly rather than reporting success.
set -euo pipefail

name="$1"; pidfile="$2"; logfile="$3"; shift 3
if [ "${1:-}" != "--" ]; then
  echo "spawn-daemon: expected -- before command" >&2
  exit 2
fi
shift

mkdir -p "$(dirname "$pidfile")" "$(dirname "$logfile")"
# Truncate up-front so a tail-on-crash shows ONLY this run (not a stale log).
: > "$logfile"

# Detach into a brand-new session / process group so no SIGHUP from the
# exiting `just`/recipe shell can reach the child, and `< /dev/null` severs
# stdin. `setsid` is the clean primitive but is NOT present on base macOS
# (no gsetsid either on the dev boxes here), so fall back to a subshell that
# ignores HUP and is `disown`ed from the job table.
if command -v setsid >/dev/null 2>&1; then
  setsid "$@" < /dev/null >> "$logfile" 2>&1 &
  child=$!
else
  # Portable fallback: new-ish session via nohup-style HUP-ignore + disown.
  ( trap '' HUP; exec "$@" ) < /dev/null >> "$logfile" 2>&1 &
  child=$!
  disown "$child" 2>/dev/null || true
fi

echo "$child" > "$pidfile"

# Early-crash gate: give the binary a moment to bind its port / connect to
# pg+nats / panic. If it's already gone, the launch FAILED — drop the
# misleading pidfile and surface the captured log loudly.
sleep 1
if ! kill -0 "$child" 2>/dev/null; then
  rm -f "$pidfile"
  {
    echo "✗ $name died within 1s of launch — last 40 log lines ($logfile):"
    tail -n 40 "$logfile" 2>/dev/null || echo "  (log empty — crashed before any output was flushed)"
  } >&2
  exit 1
fi

echo "$child"
