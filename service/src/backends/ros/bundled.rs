//! Bundled rosapi typedef snapshots.
//!
//! These are committed JSON captures in the exact `/rosapi/message_details`
//! wire format (an array of [`TypeDef`]) for the turtlesim demo interfaces.
//! They are GROUND TRUTH — what the live rosbridge at the demo turtlesim
//! container returns verbatim — so the deriver can produce a typed output port
//! offline (no rosbridge round-trip per editor keystroke).
//!
//! Keyed by the **message-details `type` form** (no `/msg/` infix; services use
//! `_Request` / `_Response`). The deriver normalizes the authored
//! `interface_type` + operation into the lookup key.
//!
//! In P3 the runner's live interface catalog supersedes these for arbitrary
//! interfaces; the bundle remains the offline fallback for the demo surface.

use super::typedef::TypeDef;

/// One bundled snapshot: a logical key → the raw `message_details` JSON.
struct Snapshot {
    key: &'static str,
    json: &'static str,
}

/// All bundled snapshots. Keys are the resolved root type as it appears in the
/// rosapi `type` field. Action goal/result/feedback use the
/// `pkg/Type_{Goal,Result,Feedback}` convention.
static SNAPSHOTS: &[Snapshot] = &[
    Snapshot {
        key: "geometry_msgs/Twist",
        json: include_str!("bundled/geometry_msgs__Twist.json"),
    },
    Snapshot {
        key: "turtlesim/Pose",
        json: include_str!("bundled/turtlesim__Pose.json"),
    },
    Snapshot {
        key: "turtlesim/TeleportAbsolute_Request",
        json: include_str!("bundled/turtlesim__TeleportAbsolute_Request.json"),
    },
    Snapshot {
        key: "turtlesim/TeleportAbsolute_Response",
        json: include_str!("bundled/turtlesim__TeleportAbsolute_Response.json"),
    },
    Snapshot {
        key: "turtlesim/Spawn_Request",
        json: include_str!("bundled/turtlesim__Spawn_Request.json"),
    },
    Snapshot {
        key: "turtlesim/Spawn_Response",
        json: include_str!("bundled/turtlesim__Spawn_Response.json"),
    },
    Snapshot {
        key: "turtlesim/RotateAbsolute_Goal",
        json: include_str!("bundled/turtlesim__RotateAbsolute_Goal.json"),
    },
    Snapshot {
        key: "turtlesim/RotateAbsolute_Result",
        json: include_str!("bundled/turtlesim__RotateAbsolute_Result.json"),
    },
    Snapshot {
        key: "turtlesim/RotateAbsolute_Feedback",
        json: include_str!("bundled/turtlesim__RotateAbsolute_Feedback.json"),
    },
];

/// Look up the bundled typedef list for a resolved root `key` (in
/// message-details `type` form). Returns `None` for an unknown interface — the
/// deriver treats that as "produce an empty port" (permissive, never errors).
pub fn lookup(key: &str) -> Option<Vec<TypeDef>> {
    let snap = SNAPSHOTS.iter().find(|s| s.key == key)?;
    serde_json::from_str(snap.json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_snapshot_parses() {
        for snap in SNAPSHOTS {
            let parsed: Result<Vec<TypeDef>, _> = serde_json::from_str(snap.json);
            assert!(parsed.is_ok(), "snapshot {} must parse", snap.key);
        }
    }

    #[test]
    fn lookup_known_and_unknown() {
        assert!(lookup("geometry_msgs/Twist").is_some());
        assert!(lookup("turtlesim/Spawn_Response").is_some());
        assert!(lookup("no/such/Thing").is_none());
    }
}
