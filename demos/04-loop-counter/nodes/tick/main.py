"""Loop body — prints + emits the current iteration counter.

`lp` is the loop's slug; reading `lp.iteration` works because the compiler
detects the cross-node reference, stages the parked counter envelope as
`lp.json`, and the runner promotes it as a global. No SDK init needed.
"""

log_info(f"loop iteration {lp.iteration}")
saw = lp.iteration
