use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum byte length for Unix domain socket paths.
/// macOS `sun_path` is 104 bytes (103 usable + NUL), Linux is 108.
/// Use the stricter macOS limit for cross-platform safety.
const UNIX_SOCKET_PATH_MAX: usize = 103;

/// Pure path computation for the run directory layout.
///
/// Each execution gets a structured directory tree. This type computes
/// all paths from the root — it performs no I/O.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunDirectory {
    /// Root directory: `{base_dir}/runs/{execution_id}/`
    pub root: PathBuf,

    /// Serialized RunContext (read-only for child): `{root}/context.json`
    pub context_file: PathBuf,

    /// Staged input files: `{root}/inputs/`
    pub inputs_dir: PathBuf,

    /// Child writes declared outputs here: `{root}/outputs/`
    pub outputs_dir: PathBuf,

    /// Child writes artifact files here: `{root}/artifacts/`
    pub artifacts_dir: PathBuf,

    /// Log files (stdout.log, stderr.log): `{root}/logs/`
    pub logs_dir: PathBuf,

    /// IPC Unix domain socket.
    ///
    /// Normally `{root}/ipc.sock`, but when the path would exceed the
    /// Unix `sun_path` limit (103 bytes on macOS) it is placed under a
    /// short hashed directory in `/tmp` instead.
    pub ipc_socket: PathBuf,
}

impl RunDirectory {
    /// Compute all paths for a given execution under `base_dir`.
    pub fn new(base_dir: &Path, execution_id: &str) -> Self {
        let root = base_dir.join("runs").join(execution_id);
        let natural_socket = root.join("ipc.sock");

        // Unix domain sockets have a hard path length limit (sun_path).
        // With compound execution IDs (e.g. two UUIDs) the natural path
        // can exceed this. Fall back to a short hashed path in /tmp.
        let ipc_socket = if natural_socket.as_os_str().len() <= UNIX_SOCKET_PATH_MAX {
            natural_socket
        } else {
            let mut hasher = DefaultHasher::new();
            root.hash(&mut hasher);
            let hash = hasher.finish();
            // /tmp/.aex/{16-hex-chars}/ipc.sock = ~38 chars — well within limit
            PathBuf::from(format!("/tmp/.aex/{hash:016x}/ipc.sock"))
        };

        Self {
            context_file: root.join("context.json"),
            inputs_dir: root.join("inputs"),
            outputs_dir: root.join("outputs"),
            artifacts_dir: root.join("artifacts"),
            logs_dir: root.join("logs"),
            ipc_socket,
            root,
        }
    }

    /// All directories that need to be created before execution.
    pub fn all_dirs(&self) -> Vec<&PathBuf> {
        vec![
            &self.root,
            &self.inputs_dir,
            &self.outputs_dir,
            &self.artifacts_dir,
            &self.logs_dir,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_directory_paths() {
        let base = PathBuf::from("/data/executor");
        let dir = RunDirectory::new(&base, "exec-123");

        assert_eq!(dir.root, PathBuf::from("/data/executor/runs/exec-123"));
        assert_eq!(
            dir.context_file,
            PathBuf::from("/data/executor/runs/exec-123/context.json")
        );
        assert_eq!(
            dir.inputs_dir,
            PathBuf::from("/data/executor/runs/exec-123/inputs")
        );
        assert_eq!(
            dir.outputs_dir,
            PathBuf::from("/data/executor/runs/exec-123/outputs")
        );
        assert_eq!(
            dir.artifacts_dir,
            PathBuf::from("/data/executor/runs/exec-123/artifacts")
        );
        assert_eq!(
            dir.logs_dir,
            PathBuf::from("/data/executor/runs/exec-123/logs")
        );
        // Short execution ID → socket stays in run dir
        assert_eq!(
            dir.ipc_socket,
            PathBuf::from("/data/executor/runs/exec-123/ipc.sock")
        );
    }

    #[test]
    fn long_execution_id_gets_short_socket_path() {
        // Simulate spawned-net execution IDs: two UUIDs joined by a dash
        let base = PathBuf::from("/tmp/invoice-processing-executor");
        let long_id =
            "9db811ef-77cb-434a-9b9d-740a87b3f14d-17e0fbf3-b5bc-4650-ae05-cb44873e1b85";

        let dir = RunDirectory::new(&base, long_id);

        // Run dir uses full ID (filesystem has no path-length issue)
        assert!(dir.root.to_string_lossy().contains(long_id));

        // IPC socket must be under the Unix sun_path limit
        let socket_len = dir.ipc_socket.as_os_str().len();
        assert!(
            socket_len <= UNIX_SOCKET_PATH_MAX,
            "IPC socket path too long: {} chars (max {UNIX_SOCKET_PATH_MAX}): {}",
            socket_len,
            dir.ipc_socket.display()
        );

        // Socket should be in the /tmp/.aex/ fallback directory
        assert!(
            dir.ipc_socket.starts_with("/tmp/.aex/"),
            "Expected short socket path, got: {}",
            dir.ipc_socket.display()
        );
    }

    #[test]
    fn short_socket_path_is_deterministic() {
        let base = PathBuf::from("/tmp/my-executor");
        let long_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee-ffffffff-1111-2222-3333-444444444444";

        let dir1 = RunDirectory::new(&base, long_id);
        let dir2 = RunDirectory::new(&base, long_id);

        assert_eq!(dir1.ipc_socket, dir2.ipc_socket);
    }

    #[test]
    fn all_dirs_contains_expected() {
        let base = PathBuf::from("/tmp/exec");
        let dir = RunDirectory::new(&base, "test");
        let dirs = dir.all_dirs();
        assert_eq!(dirs.len(), 5);
        assert!(dirs.contains(&&dir.root));
        assert!(dirs.contains(&&dir.inputs_dir));
        assert!(dirs.contains(&&dir.outputs_dir));
        assert!(dirs.contains(&&dir.artifacts_dir));
        assert!(dirs.contains(&&dir.logs_dir));
    }

    #[test]
    fn run_directory_serde_roundtrip() {
        let base = PathBuf::from("/data/executor");
        let dir = RunDirectory::new(&base, "exec-456");
        let json = serde_json::to_string(&dir).unwrap();
        let deserialized: RunDirectory = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.root, dir.root);
        assert_eq!(deserialized.ipc_socket, dir.ipc_socket);
    }
}
