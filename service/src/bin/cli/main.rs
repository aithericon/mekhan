mod cancel;
mod diff;
mod doc_ops;
mod formats;
mod fs_ops;
mod init;
mod instances;
mod list;
mod logs;
mod publish;
mod pull;
mod push;
mod run;
mod status;
mod ws_client;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mekhan", about = "Mekhan workflow CLI — import/export templates")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Server URL (e.g. http://localhost:3100)
    #[arg(short, long, default_value = "http://localhost:3100", global = true)]
    server: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new template
    Init {
        /// Template name
        name: String,

        /// Optional description
        #[arg(short, long)]
        description: Option<String>,

        /// Output format: json, yaml, or hcl (default: yaml)
        #[arg(short, long, default_value = "yaml")]
        format: String,
    },

    /// List available templates
    List,

    /// Pull a template to a local directory
    Pull {
        /// Template ID (UUID)
        template_id: String,

        /// Target directory (defaults to template name)
        #[arg(short, long)]
        directory: Option<String>,

        /// Output format: json, yaml, or hcl (defaults to yaml for new, stored format for existing)
        #[arg(long)]
        format: Option<String>,
    },

    /// Push local changes back to the server
    Push {
        /// Directory containing the template (defaults to current directory)
        #[arg(default_value = ".")]
        directory: String,

        /// Dry run — show diff without applying
        #[arg(long)]
        dry_run: bool,
    },

    /// Show diff between local and remote (like push --dry-run)
    Status {
        /// Directory containing the template (defaults to current directory)
        #[arg(default_value = ".")]
        directory: String,
    },

    /// Publish a template (compile to AIR and freeze)
    Publish {
        /// Directory containing the template (defaults to current directory)
        #[arg(default_value = ".")]
        directory: String,
    },

    /// Create a new workflow instance from a published template
    Run {
        /// Directory containing the template (defaults to current directory)
        #[arg(default_value = ".")]
        directory: String,
    },

    /// List workflow instances
    Instances {
        /// Filter by template ID
        #[arg(long)]
        template: Option<String>,
    },

    /// Cancel a running workflow instance
    Cancel {
        /// Instance ID (UUID)
        instance_id: String,
    },

    /// View instance state, events, and marking
    Logs {
        /// Instance ID (UUID)
        instance_id: String,

        /// Show only the last N events
        #[arg(short, long)]
        tail: Option<usize>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init {
            name,
            description,
            format,
        } => {
            let fmt = formats::WorkflowFormat::from_str_arg(&format)?;
            init::run(&cli.server, &name, description.as_deref(), fmt).await
        }
        Commands::List => list::run(&cli.server).await,
        Commands::Pull {
            template_id,
            directory,
            format,
        } => {
            let fmt = match format {
                Some(f) => formats::WorkflowFormat::from_str_arg(&f)?,
                None => formats::WorkflowFormat::Yaml,
            };
            pull::run(&cli.server, &template_id, directory.as_deref(), fmt).await
        }
        Commands::Push {
            directory,
            dry_run,
        } => push::run(&cli.server, &directory, dry_run).await,
        Commands::Status { directory } => status::run(&cli.server, &directory).await,
        Commands::Publish { directory } => publish::run(&cli.server, &directory).await,
        Commands::Run { directory } => run::run(&cli.server, &directory).await,
        Commands::Instances { template } => {
            instances::run(&cli.server, template.as_deref()).await
        }
        Commands::Cancel { instance_id } => cancel::run(&cli.server, &instance_id).await,
        Commands::Logs { instance_id, tail } => {
            logs::run(&cli.server, &instance_id, tail).await
        }
    }
}
