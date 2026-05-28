# Branch A — uppercase the inbound subject.
#
# `input.subject` is the Start field. Both A and B see the same control
# token because ParallelSplit forks rather than partitions.

shouted = (input.subject or "").upper()
log_info("branch A done", shouted=shouted)
