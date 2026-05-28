# lookup_order — the Python tool the Triage Agent can call.
#
# `input.order_id` is the argument the LLM emitted in its tool call.
# The agent compiler wires the LLM's `tool_calls[0].arguments` map
# straight into this child's input port, so the LLM key (`order_id`)
# must match the input port field name above.
#
# Returns a mock status. A real implementation would hit a database /
# shipping API here. The two declared outputs (`status`, `eta`) are
# swept by the Python runner from globals at exit and packaged back
# into a `role: tool` message the agent feeds to the next LLM turn.

oid = input.order_id

mock_orders = {
    "ORD-42":  ("In transit",      "tomorrow"),
    "ORD-100": ("Out for delivery", "today"),
    "ORD-7":   ("Delivered",        "yesterday"),
}

status, eta = mock_orders.get(oid, ("Unknown order id", "n/a"))

log_info("looked up order", order_id=oid, status=status)
