# Always fails. The error handle is unwired in graph.json, so once retries
# are exhausted (maxRetries == 0 → immediately) the engine must crash the
# net (NetFailed) rather than strand the failure token in a dead-end place.
raise RuntimeError(f"Synthetic unhandled failure: payload={input.payload!r}")
