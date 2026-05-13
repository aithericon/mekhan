mod cli;
mod commands;
mod connection;
mod error;
mod output;

use std::process::ExitCode;

use clap::Parser;

use cli::{ArtifactCmd, Commands, InputsCmd, MetricCmd, OutputCmd, PhaseCmd, ProgressCmd};
use error::CliError;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    let json = cli.json;

    let result = run(cli).await;

    match result {
        Ok(()) => {
            let msg = output::format_ok(json);
            if !msg.is_empty() {
                println!("{msg}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let msg = output::format_error(json, &e);
            eprintln!("{msg}");
            e.exit_code()
        }
    }
}

async fn run(cli: cli::Cli) -> Result<(), CliError> {
    // Commands that don't need a gRPC connection.
    match &cli.command {
        Commands::Inputs { cmd } => {
            return match cmd {
                InputsCmd::List => commands::inputs::list_inputs(cli.json),
                InputsCmd::Get { name } => commands::inputs::get_input(name, cli.json),
            };
        }
        // output set falls back to file-based when no socket is available.
        Commands::Output {
            cmd:
                OutputCmd::Set {
                    name,
                    value,
                    raw,
                    stdin,
                },
        } if cli.socket.is_none() => {
            let raw_value = if *stdin {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                value.clone().ok_or_else(|| {
                    CliError::InvalidArgument("value required (or use --stdin)".into())
                })?
            };
            return commands::output_cmd::set_output_fallback(name, &raw_value, *raw);
        }
        _ => {}
    }

    // All other commands require a connection.
    let socket = cli.socket.as_deref().ok_or(CliError::NoSocket)?;
    let mut client = connection::connect(socket).await?;

    match cli.command {
        Commands::Output { cmd } => match cmd {
            OutputCmd::Set {
                name,
                value,
                raw,
                stdin,
            } => commands::output_cmd::set_output(&mut client, name, value, raw, stdin).await,
        },
        Commands::Artifact { cmd } => match cmd {
            ArtifactCmd::Log {
                path,
                name,
                category,
                mime_type,
                metadata,
                extract_metadata,
            } => {
                commands::artifact::log_artifact(
                    &mut client,
                    path,
                    name,
                    category,
                    mime_type,
                    metadata,
                    extract_metadata,
                )
                .await
            }
        },
        Commands::Progress { cmd } => match cmd {
            ProgressCmd::Update {
                fraction,
                message,
                step,
                total_steps,
            } => {
                commands::progress::update_progress(
                    &mut client,
                    fraction,
                    message,
                    step,
                    total_steps,
                )
                .await
            }
        },
        Commands::Phase { cmd } => match cmd {
            PhaseCmd::Define { names } => commands::phase::define_phases(&mut client, names).await,
            PhaseCmd::Update {
                name,
                status,
                message,
            } => commands::phase::update_phase(&mut client, name, status, message).await,
        },
        Commands::Log {
            level,
            message,
            fields,
        } => commands::log_cmd::log_message(&mut client, level, message, fields).await,
        Commands::Metric { cmd } => match cmd {
            MetricCmd::Log {
                name,
                value,
                step,
                metric_type,
                labels,
            } => {
                commands::metric::log_metric(&mut client, name, value, step, metric_type, labels)
                    .await
            }
            MetricCmd::Batch => commands::metric::log_metrics_batch(&mut client).await,
        },
        Commands::Health => commands::health::health_check(&mut client).await,
        Commands::Shutdown { exit_code } => {
            commands::shutdown::shutdown(&mut client, exit_code).await
        }
        Commands::Inputs { .. } => unreachable!(),
    }
}
