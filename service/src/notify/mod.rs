//! Outbound notifications (Phase 4). Currently just invite email; the trait
//! seam keeps delivery pluggable (log / SMTP) and offline-friendly by default.

pub mod email;
