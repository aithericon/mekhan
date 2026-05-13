use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "aithericon",
    about = "CLI client for the Aithericon executor IPC sidecar"
)]
pub struct Cli {
    /// Path to the IPC Unix socket (overrides AITHERICON_IPC_SOCKET).
    #[arg(long = "socket", global = true, env = "AITHERICON_IPC_SOCKET")]
    pub socket: Option<String>,

    /// Emit JSON output for programmatic consumption.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage execution outputs.
    Output {
        #[command(subcommand)]
        cmd: OutputCmd,
    },
    /// Log artifact files.
    Artifact {
        #[command(subcommand)]
        cmd: ArtifactCmd,
    },
    /// Report execution progress.
    Progress {
        #[command(subcommand)]
        cmd: ProgressCmd,
    },
    /// Define and update execution phases.
    Phase {
        #[command(subcommand)]
        cmd: PhaseCmd,
    },
    /// Send structured log messages.
    Log {
        /// Log level.
        level: LogLevelArg,
        /// Log message.
        message: String,
        /// Structured fields as KEY=VALUE pairs.
        #[arg(long = "field", value_name = "KEY=VALUE")]
        fields: Vec<String>,
    },
    /// Log metrics.
    Metric {
        #[command(subcommand)]
        cmd: MetricCmd,
    },
    /// Send a health check ping to the sidecar.
    Health,
    /// Send shutdown acknowledgment to the sidecar.
    Shutdown {
        /// Exit code to report.
        #[arg(long, default_value = "0")]
        exit_code: i32,
    },
    /// Read staged input files.
    Inputs {
        #[command(subcommand)]
        cmd: InputsCmd,
    },
}

// -- Output --

#[derive(Subcommand)]
pub enum OutputCmd {
    /// Set a named output value.
    Set {
        /// Output name.
        name: String,
        /// Output value (JSON). Omit if using --stdin.
        value: Option<String>,
        /// Treat the value as a raw string, not JSON.
        #[arg(long)]
        raw: bool,
        /// Read value from stdin.
        #[arg(long)]
        stdin: bool,
    },
}

// -- Artifact --

#[derive(Subcommand)]
pub enum ArtifactCmd {
    /// Log an artifact file.
    Log {
        /// Path to the artifact file.
        path: String,
        /// Display name (defaults to filename).
        #[arg(long)]
        name: Option<String>,
        /// Artifact category.
        #[arg(long, default_value = "other")]
        category: ArtifactCategoryArg,
        /// MIME type.
        #[arg(long)]
        mime_type: Option<String>,
        /// Metadata key-value pairs (KEY=VALUE).
        #[arg(long = "metadata", value_name = "KEY=VALUE")]
        metadata: Vec<String>,
        /// Request sidecar to extract file metadata.
        #[arg(long)]
        extract_metadata: bool,
    },
}

// -- Progress --

#[derive(Subcommand)]
pub enum ProgressCmd {
    /// Update execution progress.
    Update {
        /// Progress fraction (0.0 to 1.0).
        fraction: f32,
        /// Human-readable progress message.
        #[arg(long)]
        message: Option<String>,
        /// Current step number.
        #[arg(long)]
        step: Option<u64>,
        /// Total number of steps.
        #[arg(long)]
        total_steps: Option<u64>,
    },
}

// -- Phase --

#[derive(Subcommand)]
pub enum PhaseCmd {
    /// Define execution phases upfront.
    Define {
        /// Phase names.
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Update a phase's status.
    Update {
        /// Phase name.
        name: String,
        /// New status.
        status: PhaseStatusArg,
        /// Optional status message.
        #[arg(long)]
        message: Option<String>,
    },
}

// -- Metric --

#[derive(Subcommand)]
pub enum MetricCmd {
    /// Log a single metric point.
    Log {
        /// Metric name (e.g. "train/loss").
        name: String,
        /// Metric value.
        value: f64,
        /// Training step / iteration.
        #[arg(long)]
        step: Option<u64>,
        /// Metric type.
        #[arg(long = "type", default_value = "scalar")]
        metric_type: MetricTypeArg,
        /// Labels as KEY=VALUE pairs.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
    },
    /// Log a batch of metrics from stdin (JSON array).
    Batch,
}

// -- Inputs --

#[derive(Subcommand)]
pub enum InputsCmd {
    /// List staged input files.
    List,
    /// Read a specific input file to stdout.
    Get {
        /// Input file name.
        name: String,
    },
}

// -- Value enums --

#[derive(Clone, ValueEnum)]
pub enum LogLevelArg {
    Info,
    Trace,
    Debug,
    Warn,
    Error,
}

#[derive(Clone, ValueEnum)]
pub enum PhaseStatusArg {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

#[derive(Clone, ValueEnum)]
pub enum ArtifactCategoryArg {
    Other,
    Model,
    Dataset,
    Plot,
    Log,
    Checkpoint,
    Config,
    Metric,
}

#[derive(Clone, ValueEnum)]
pub enum MetricTypeArg {
    Scalar,
    Counter,
    Gauge,
    Histogram,
}
