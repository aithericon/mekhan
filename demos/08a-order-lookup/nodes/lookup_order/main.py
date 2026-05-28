# Look up order — the Python work behind the order-lookup tool.
#
# When this template is referenced as an Agent tool (09-agent-tool-loop),
# the LLM's tool-call arguments arrive as this child net's Start token, so
# `input.order_id` here is the `order_id` the LLM emitted. (Outside the
# agent, `order_id` is just the Start input field like any other workflow.)
#
# Returns a mock status. A real implementation would hit a database /
# shipping API here. The two declared outputs (`status`, `eta`) are swept
# by the Python runner from globals at exit; the End node maps them back as
# the tool result the agent feeds to its next LLM turn.

oid = input.order_id

mock_orders = {
    "ORD-42":  ("In transit",      "tomorrow"),
    "ORD-100": ("Out for delivery", "today"),
    "ORD-7":   ("Delivered",        "yesterday"),
}

status, eta = mock_orders.get(oid, ("Unknown order id", "n/a"))

log_info("looked up order", order_id=oid, status=status)
