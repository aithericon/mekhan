//! Per-surface borrow planners. Each module owns the scan + resolve
//! step for one authoring surface; the unified [`super::collect_borrows`]
//! chains them.

pub(crate) mod automated_step;
pub(crate) mod guard;
pub(crate) mod human_task;
pub(crate) mod resource;
