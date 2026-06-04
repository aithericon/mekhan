//! Per-replica admission.
//!
//! Each replica carries a `tokio::sync::Semaphore` sized to its per-engine
//! concurrency `C` (vLLM `--max-num-seqs`). Admission is a non-blocking
//! `try_acquire` — on success the permit is held for the **entire** request
//! lifetime (the full SSE stream) and released on completion, client
//! disconnect, or cancellation; on saturation the caller returns `429` +
//! `Retry-After`.
//!
//! This is the router's ONLY concurrency authority for inference. We never
//! gate inference through the engine Petri net / presence-pool — that would
//! serialize requests in front of vLLM's continuous batcher and tank
//! throughput (doc 28 §5/§6). The semaphore only caps how many concurrent
//! requests we hand each engine; vLLM does the real scheduling.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Try to admit a request against a replica's slot budget. Returns the held
/// permit, or `None` when the replica is saturated (→ caller emits `429`).
pub fn try_admit(sem: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    sem.clone().try_acquire_owned().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_capacity_then_rejects() {
        let sem = Arc::new(Semaphore::new(2));
        let p1 = try_admit(&sem);
        let p2 = try_admit(&sem);
        assert!(p1.is_some() && p2.is_some());
        // Third is rejected while the first two are held.
        assert!(try_admit(&sem).is_none());
        // Releasing one frees a slot.
        drop(p1);
        assert!(try_admit(&sem).is_some());
    }
}
