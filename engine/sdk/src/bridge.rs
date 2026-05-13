//! Typed bridge addressing for compile-time checked cross-net wiring.
//!
//! Instead of raw string pairs for net_id + place_name:
//! ```ignore
//! ctx.bridge_out::<T>("to_jobs", "To Jobs", "job-net", "job_queue");
//! ```
//!
//! Use typed constants:
//! ```ignore
//! use my_interfaces::JOB_QUEUE;
//! ctx.bridge_out_to::<T>("to_jobs", "To Jobs", &JOB_QUEUE);
//! ```
//!
//! Define constants in shared interface modules so both sides of a bridge agree:
//! ```ignore
//! pub const JOB_QUEUE: BridgeAddress = BridgeAddress::new("job-net", "job_queue");
//! ```

/// A typed reference to a place in a remote net.
///
/// Use as `const` in interface modules shared between nets.
/// The `net_id` is the default (overridable at deploy time via `--net-id`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeAddress {
    pub net_id: &'static str,
    pub place_name: &'static str,
}

impl BridgeAddress {
    pub const fn new(net_id: &'static str, place_name: &'static str) -> Self {
        Self { net_id, place_name }
    }
}

/// Target for bridge-out operations.
#[derive(Clone, Debug)]
pub struct BridgeTarget {
    pub net_id: String,
    pub place_name: String,
}

impl From<&BridgeAddress> for BridgeTarget {
    fn from(addr: &BridgeAddress) -> Self {
        Self {
            net_id: addr.net_id.to_string(),
            place_name: addr.place_name.to_string(),
        }
    }
}

impl From<(&str, &str)> for BridgeTarget {
    fn from((net_id, place_name): (&str, &str)) -> Self {
        Self {
            net_id: net_id.to_string(),
            place_name: place_name.to_string(),
        }
    }
}

/// Source annotation for bridge-in places.
#[derive(Clone, Debug)]
pub struct BridgeSource {
    pub net_id: String,
    pub place_name: String,
}

impl From<&BridgeAddress> for BridgeSource {
    fn from(addr: &BridgeAddress) -> Self {
        Self {
            net_id: addr.net_id.to_string(),
            place_name: addr.place_name.to_string(),
        }
    }
}

impl From<(&str, &str)> for BridgeSource {
    fn from((net_id, place_name): (&str, &str)) -> Self {
        Self {
            net_id: net_id.to_string(),
            place_name: place_name.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_construction() {
        const ADDR: BridgeAddress = BridgeAddress::new("my-net", "my_place");
        assert_eq!(ADDR.net_id, "my-net");
        assert_eq!(ADDR.place_name, "my_place");
    }

    #[test]
    fn into_target_from_address() {
        const ADDR: BridgeAddress = BridgeAddress::new("net-a", "place_b");
        let target: BridgeTarget = (&ADDR).into();
        assert_eq!(target.net_id, "net-a");
        assert_eq!(target.place_name, "place_b");
    }

    #[test]
    fn into_target_from_tuple() {
        let target: BridgeTarget = ("net-a", "place_b").into();
        assert_eq!(target.net_id, "net-a");
        assert_eq!(target.place_name, "place_b");
    }

    #[test]
    fn into_source_from_address() {
        const ADDR: BridgeAddress = BridgeAddress::new("net-x", "place_y");
        let source: BridgeSource = (&ADDR).into();
        assert_eq!(source.net_id, "net-x");
        assert_eq!(source.place_name, "place_y");
    }
}
