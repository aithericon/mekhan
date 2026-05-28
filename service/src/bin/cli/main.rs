mod apply;
mod cancel;
mod diff;
mod doc_ops;
mod formats;
mod fs_ops;
mod http;
mod init;
mod instances;
mod list;
mod logs;
mod publish;
mod pull;
mod push;
mod run;
mod status;
mod test_cmd;
mod tests_fs;
mod ws_client;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mekhan", about = "Mekhan workflow CLI — import/export templates")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Server URL (e.g. http://localhost:13100)
    #[arg(short, long, default_value = "http://localhost:13100", global = true)]
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

    /// GitOps: atomically version + publish a template from the local
    /// git-authored artifact, recording git provenance. The local graph
    /// REPLACES the chain head (no collaborative merge).
    Apply {
        /// Directory containing the template (defaults to current directory)
        #[arg(default_value = ".")]
        directory: String,
    },

    /// Create a new workflow instance from a published template.
    ///
    /// Argument is either a template UUID (instantiates that template on
    /// `--server`) or a path to a `mekhan.lock.json`-bearing directory (uses
    /// the directory's pinned `server_url` + `template_id`). Defaults to
    /// the current directory.
    Run {
        #[arg(default_value = ".")]
        template: String,

        /// Seed a Start block's `initial` port. Repeatable. Format:
        /// `<start_block_id>.<field>=<value>`. `<value>` is parsed as JSON
        /// (`42`, `true`, `"x"`, `{...}`) and falls back to a bare string.
        /// Mutually exclusive with `--start-tokens`.
        #[arg(short = 'i', long = "input", value_name = "BLOCK.FIELD=VALUE")]
        inputs: Vec<String>,

        /// Path to a JSON file containing the full `start_tokens` array
        /// (matches the test-fixture shape). Mutually exclusive with `-i`.
        #[arg(long = "start-tokens", value_name = "PATH", conflicts_with = "inputs")]
        start_tokens_file: Option<String>,
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

    /// Print the OpenAPI 3 spec to stdout (no DB or NATS required).
    /// Used by the frontend codegen pipeline to regenerate
    /// `app/src/lib/api/v1/schema.d.ts`.
    Openapi,

    /// Run template tests against the latest published version of a
    /// template family. Exit code 0 only when every enabled test passes.
    Test {
        /// Either a template UUID or a directory holding `mekhan.lock.json`.
        template: String,

        /// Run only the test with this name (otherwise runs every enabled
        /// test).
        #[arg(short, long)]
        name: Option<String>,

        /// Include disabled tests in the run (default skips them).
        #[arg(long)]
        include_disabled: bool,
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
        Commands::Apply { directory } => apply::run(&cli.server, &directory).await,
        Commands::Run {
            template,
            inputs,
            start_tokens_file,
        } => run::run(&cli.server, &template, &inputs, start_tokens_file.as_deref()).await,
        Commands::Instances { template } => {
            instances::run(&cli.server, template.as_deref()).await
        }
        Commands::Cancel { instance_id } => cancel::run(&cli.server, &instance_id).await,
        Commands::Logs { instance_id, tail } => {
            logs::run(&cli.server, &instance_id, tail).await
        }
        Commands::Openapi => {
            let spec = mekhan_service::openapi_spec();
            let json = spec
                .to_pretty_json()
                .map_err(|e| anyhow::anyhow!("serialize openapi: {e}"))?;
            println!("{json}");
            Ok(())
        }
        Commands::Test {
            template,
            name,
            include_disabled,
        } => {
            test_cmd::run(
                &cli.server,
                &template,
                name.as_deref(),
                include_disabled,
            )
            .await
        }
    }
}
