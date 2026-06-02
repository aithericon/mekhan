//! SSH session wrapper around `openssh::Session`.
//!
//! Provides a persistent ControlMaster connection to the Slurm login node.
//! All CLI commands are multiplexed over this single connection.
//! Keepalive is enabled to detect dead connections early.

use std::time::Duration;

use openssh::{KnownHosts, Session, SessionBuilder};

use crate::config::SlurmConfig;

/// Errors from SSH operations.
#[derive(Debug, thiserror::Error)]
pub enum SshError {
    /// Failed to establish the SSH session.
    #[error("SSH connection failed: {0}")]
    Connection(#[from] openssh::Error),

    /// Remote command failed with a non-zero exit code.
    #[error("Remote command failed (exit {exit_code}): {stderr}")]
    CommandFailed {
        exit_code: i32,
        stderr: String,
        stdout: String,
    },
}

/// Persistent SSH session wrapping an `openssh::Session`.
///
/// Uses ControlMaster multiplexing for efficient command execution
/// over a single TCP connection. ServerAliveInterval is set to detect
/// dead connections within ~45 seconds (15s interval × 3 missed).
///
/// `command_timeout` caps how long an individual remote command can hang
/// before we give up on this session entirely. This handles the case where
/// the ControlMaster master process has died (e.g. after laptop sleep) but
/// our slave subprocess hangs forever on the now-dead Unix-domain socket —
/// `server_alive_interval` doesn't help here because the slave isn't on a
/// live channel to send keepalives over. On timeout we surface
/// `SshError::Connection(Disconnected)` so the caller's reconnect path
/// (e.g. `SlurmClient::exec_with_reconnect`) rebuilds the session.
pub struct SshSession {
    session: Session,
    command_timeout: Duration,
}

impl SshSession {
    /// Establish a new SSH connection using the provided config.
    pub async fn connect(config: &SlurmConfig) -> Result<Self, SshError> {
        let mut builder = SessionBuilder::default();

        builder.port(config.ssh_port);

        // Configure known_hosts checking
        let known_hosts = match config.ssh_known_hosts.as_str() {
            "accept" => KnownHosts::Accept,
            "add" => KnownHosts::Add,
            _ => KnownHosts::Strict, // "strict" or any other value
        };
        builder.known_hosts_check(known_hosts);

        // Set the private key path
        let key_path = shellexpand::tilde(&config.ssh_key).to_string();
        builder.keyfile(&key_path);

        // Enable ControlMaster multiplexing. The control socket is a Unix domain
        // socket whose path is capped at ~104 bytes (`sockaddr_un.sun_path`) on
        // macOS/BSD. openssh derives it as `<control_dir>/.ssh-connection<rand>/
        // <user>@<host>:<port>` (+ a temp suffix), so a long `control_dir` blows
        // the limit and the master fails with "ControlPath too long", which the
        // `openssh` crate surfaces as the opaque "failed to connect to the remote
        // host". `std::env::temp_dir()` is exactly such a long path on macOS (the
        // per-user `/var/folders/…/T/` — longer still under nix-shell's
        // `$TMPDIR`). Prefer a short, stable base (`/tmp`) so the socket path
        // stays well under the cap; fall back to `temp_dir()` only if `/tmp` is
        // unavailable.
        let control_dir = {
            let short = std::path::Path::new("/tmp");
            if short.is_dir() {
                short.to_path_buf()
            } else {
                std::env::temp_dir()
            }
        };
        builder.control_directory(control_dir);

        // Keepalive: send a probe every 15s, disconnect after 3 missed replies.
        // Without this, a dead connection hangs until the next command times out
        // (potentially minutes), blocking the watcher or causing stale sbatch errors.
        builder.server_alive_interval(Duration::from_secs(15));
        builder.connect_timeout(Duration::from_secs(10));

        let destination = config.destination();
        tracing::info!(
            destination = %destination,
            port = config.ssh_port,
            "Establishing SSH connection"
        );

        let session = builder.connect(&destination).await?;

        tracing::info!(destination = %destination, "SSH connection established");
        Ok(Self {
            session,
            command_timeout: Duration::from_secs(config.command_timeout_secs),
        })
    }

    /// Execute a remote command and return its stdout.
    ///
    /// Returns an error if the command exits with a non-zero code, the SSH
    /// session is broken, or the command exceeds `command_timeout` (in which
    /// case we map to `SshError::Connection(Disconnected)` so the caller's
    /// reconnect path runs).
    pub async fn exec(&self, command: &str) -> Result<String, SshError> {
        tracing::debug!(command = %command, "Executing remote command");

        let mut cmd = self.session.command("bash");
        cmd.arg("-c").arg(command);

        let output = match tokio::time::timeout(self.command_timeout, cmd.output()).await {
            Ok(res) => res?,
            Err(_) => {
                tracing::warn!(
                    command = %command,
                    timeout_secs = self.command_timeout.as_secs(),
                    "SSH command exceeded timeout — likely dead multiplex; signalling reconnect"
                );
                return Err(SshError::Connection(openssh::Error::Disconnected));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            let exit_code = output.status.code().unwrap_or(-1);
            tracing::warn!(
                command = %command,
                exit_code = exit_code,
                stderr = %stderr,
                "Remote command failed"
            );
            return Err(SshError::CommandFailed {
                exit_code,
                stderr,
                stdout,
            });
        }

        tracing::trace!(
            command = %command,
            stdout_len = stdout.len(),
            "Remote command succeeded"
        );

        Ok(stdout)
    }

    /// Close the SSH session gracefully.
    pub async fn close(self) -> Result<(), SshError> {
        self.session.close().await?;
        Ok(())
    }
}
