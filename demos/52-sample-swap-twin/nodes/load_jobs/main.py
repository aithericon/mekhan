# Loader head for the sequential job Loop. assetBindings stage the whole
# `transfer_jobs` record collection as the `jobs` global (list[dict]); we re-emit
# it as `items` (the array the Loop iterates ONE record at a time) and `count`
# (the record count the loop condition + End report). A sequential Loop (not a
# concurrent Map) keeps the single arm from being driven by two jobs at once.
from aithericon import set_output

jobs_in = jobs                # injected asset global: list[dict] of transfer_jobs rows
set_output("items", jobs_in)
set_output("count", len(jobs_in))
