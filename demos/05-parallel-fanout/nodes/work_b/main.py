# Branch B — reverse the inbound subject. Concurrent with branch A.

reversed = (input.subject or "")[::-1]
log_info("branch B done", reversed=reversed)
