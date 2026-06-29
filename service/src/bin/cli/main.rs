mod apply;
mod cancel;
mod demos;
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
mod resource;
mod run;
mod status;
mod test_cmd;
mod tests_fs;
mod ws_client;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "mekhan",
    about = "Mekhan workflow CLI — import/export templates"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Server URL (e.g. http://localhost:3100). Falls back to $MEKHAN_CLI_SERVER.
    #[arg(
        short,
        long,
        env = "MEKHAN_CLI_SERVER",
        default_value = "http://localhost:3100",
        global = true
    )]
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

    /// Remove / reseed the built-in demo workflows (operator maintenance).
    ///
    /// Both actions are destructive: they cancel running instances and purge
    /// the engine nets of every *seeded* demo family. `reseed` then recreates
    /// them from the server's on-disk demos directory, overwriting edits.
    /// Requires admin of the default workspace (set `MEKHAN_CLI_TOKEN`).
    Demos {
        #[command(subcommand)]
        action: DemosAction,
    },

    /// Manage typed-credential / capacity resources (the things workflows bind
    /// at publish time: databases, LLM providers, SMTP, runner pools, …).
    ///
    /// `apply` is a path-keyed, hash-idempotent upsert built for CI: re-running
    /// the same manifest writes nothing unless the config changed. Secrets stay
    /// out of the repo — manifests hold `${VAR}` placeholders the CLI fills from
    /// the environment at apply time (CI pulls them from Vault into env first).
    Resource {
        #[command(subcommand)]
        action: ResourceAction,
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

#[derive(Subcommand)]
enum DemosAction {
    /// Remove every seeded demo family (does not re-seed).
    Reset,
    /// Remove every seeded demo family, then re-seed from the server's
    /// on-disk demos directory. Overwrites any user edits.
    Reseed,
}

#[derive(Subcommand)]
enum ResourceAction {
    /// Idempotently upsert resources from manifest files / directories / stdin,
    /// or built inline from flags. Re-running an unchanged manifest is a no-op
    /// (the server compares a content hash). `${VAR}` / `${VAR:-default}` in a
    /// manifest (and in `--set` values) is interpolated from the environment,
    /// so secrets pulled from Vault into env vars never have to touch disk.
    Apply {
        /// Manifest files, directories of `*.json`, or `-` for stdin.
        /// Optional when an inline resource is built with `--path`/`--type`.
        paths: Vec<String>,

        /// Build a single resource inline (no file). Requires `--type`.
        #[arg(long)]
        path: Option<String>,

        /// Resource type for an inline apply (e.g. `postgres`, `openai`).
        #[arg(long = "type")]
        resource_type: Option<String>,

        /// UI label for an inline apply (defaults to `--path`).
        #[arg(long)]
        display_name: Option<String>,

        /// Workspace id override (defaults to the token's workspace).
        #[arg(long)]
        workspace: Option<String>,

        /// Placement scope: workspace (default), folder, template, platform.
        #[arg(long)]
        scope_kind: Option<String>,

        /// Owner id for a folder/template scope.
        #[arg(long)]
        scope_id: Option<String>,

        /// Create/keep the resource restricted (no workspace-role floor).
        #[arg(long)]
        restricted: bool,

        /// Override / add a `config` field. Repeatable. `key=value`; the value
        /// is env-interpolated then parsed as JSON (bare string if not JSON).
        #[arg(short = 's', long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
    },

    /// List resources, optionally filtered by `--type`.
    List {
        /// Filter by resource type.
        #[arg(long = "type")]
        resource_type: Option<String>,
    },

    /// Show one resource's detail (secrets are server-redacted).
    Get {
        /// Resource UUID or `path`.
        id_or_path: String,
    },

    /// Soft-delete a resource.
    Delete {
        /// Resource UUID or `path`.
        id_or_path: String,
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
        Commands::Push { directory, dry_run } => push::run(&cli.server, &directory, dry_run).await,
        Commands::Status { directory } => status::run(&cli.server, &directory).await,
        Commands::Publish { directory } => publish::run(&cli.server, &directory).await,
        Commands::Apply { directory } => apply::run(&cli.server, &directory).await,
        Commands::Run {
            template,
            inputs,
            start_tokens_file,
        } => {
            run::run(
                &cli.server,
                &template,
                &inputs,
                start_tokens_file.as_deref(),
            )
            .await
        }
        Commands::Instances { template } => instances::run(&cli.server, template.as_deref()).await,
        Commands::Cancel { instance_id } => cancel::run(&cli.server, &instance_id).await,
        Commands::Logs { instance_id, tail } => logs::run(&cli.server, &instance_id, tail).await,
        Commands::Demos { action } => {
            let act = match action {
                DemosAction::Reset => demos::Action::Reset,
                DemosAction::Reseed => demos::Action::Reseed,
            };
            demos::run(&cli.server, act).await
        }
        Commands::Resource { action } => match action {
            ResourceAction::Apply {
                paths,
                path,
                resource_type,
                display_name,
                workspace,
                scope_kind,
                scope_id,
                restricted,
                set,
            } => {
                resource::apply(
                    &cli.server,
                    &paths,
                    path.as_deref(),
                    resource_type.as_deref(),
                    display_name.as_deref(),
                    workspace.as_deref(),
                    scope_kind.as_deref(),
                    scope_id.as_deref(),
                    restricted,
                    &set,
                )
                .await
            }
            ResourceAction::List { resource_type } => {
                resource::list(&cli.server, resource_type.as_deref()).await
            }
            ResourceAction::Get { id_or_path } => resource::get(&cli.server, &id_or_path).await,
            ResourceAction::Delete { id_or_path } => {
                resource::delete(&cli.server, &id_or_path).await
            }
        },
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
        } => test_cmd::run(&cli.server, &template, name.as_deref(), include_disabled).await,
    }
}
