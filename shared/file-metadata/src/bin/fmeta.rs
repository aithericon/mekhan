use std::path::PathBuf;

use clap::Parser;

use fmeta::{
    extract_all, extract_metadata, extract_metadata_with_preview, find_duplicates,
    ChecksumAlgorithm, ExtractAllOptions, PreviewOptions,
};

/// File metadata extraction CLI.
#[derive(Parser)]
#[command(name = "fmeta", version, about = "Extract metadata from files")]
struct Cli {
    /// Files or directories to process.
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Recurse into directories.
    #[arg(short, long)]
    recursive: bool,

    /// Maximum recursion depth.
    #[arg(long)]
    max_depth: Option<usize>,

    /// Include hidden files.
    #[arg(long)]
    include_hidden: bool,

    /// Pretty-print JSON output.
    #[arg(short, long)]
    pretty: bool,

    /// One JSON object per line (JSONL).
    #[arg(long)]
    compact: bool,

    /// Compute checksum (sha256 or blake3).
    #[arg(long, value_parser = parse_checksum_algo)]
    checksum: Option<ChecksumAlgorithm>,

    /// Include first N rows as content preview.
    #[arg(long)]
    preview: Option<usize>,

    /// Find duplicate files by checksum (requires --checksum and --recursive).
    #[arg(long)]
    find_duplicates: bool,

    /// Compute column-level statistics for tabular formats.
    #[arg(long)]
    statistics: bool,

    /// Classify columns for PII/content patterns (email, phone, etc.).
    #[arg(long)]
    classify: bool,
}

fn parse_checksum_algo(s: &str) -> Result<ChecksumAlgorithm, String> {
    match s.to_lowercase().as_str() {
        "sha256" => Ok(ChecksumAlgorithm::Sha256),
        "blake3" => Ok(ChecksumAlgorithm::Blake3),
        _ => Err(format!(
            "unknown algorithm: {s} (expected sha256 or blake3)"
        )),
    }
}

fn main() {
    let cli = Cli::parse();

    let mut exit_code = 0;

    for path in &cli.paths {
        if !path.exists() {
            eprintln!("error: path not found: {}", path.display());
            exit_code = 1;
            continue;
        }

        if path.is_dir() {
            if !cli.recursive {
                eprintln!(
                    "error: {} is a directory (use --recursive to scan)",
                    path.display()
                );
                exit_code = 1;
                continue;
            }

            let mut options = ExtractAllOptions::new();
            if let Some(depth) = cli.max_depth {
                options = options.with_max_depth(depth);
            }
            if cli.include_hidden {
                options = options.include_hidden();
            }
            if let Some(algo) = &cli.checksum {
                options = options.with_checksum(algo.clone());
            }

            match extract_all(path, &options) {
                Ok(results) => {
                    if cli.find_duplicates {
                        let dups = find_duplicates(&results);
                        let json = if cli.pretty {
                            serde_json::to_string_pretty(&dups)
                        } else {
                            serde_json::to_string(&dups)
                        };
                        match json {
                            Ok(s) => println!("{s}"),
                            Err(e) => {
                                eprintln!("error: failed to serialize duplicates: {e}");
                                exit_code = 1;
                            }
                        }
                    } else {
                        for result in results {
                            match result.result {
                                Ok(mut meta) => {
                                    if let Some(max_rows) = cli.preview {
                                        let preview_opts =
                                            PreviewOptions::new().with_max_rows(max_rows);
                                        meta.preview =
                                            fmeta::preview::extract_preview(
                                                &result.path,
                                                &meta.format,
                                                &preview_opts,
                                                meta.num_rows,
                                            );
                                    }
                                    output_meta(&meta, &cli);
                                }
                                Err(e) => {
                                    eprintln!("error: {}: {e}", result.path.display());
                                    exit_code = 1;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: {}: {e}", path.display());
                    exit_code = 1;
                }
            }
        } else {
            let result = if let Some(max_rows) = cli.preview {
                let preview_opts = PreviewOptions::new().with_max_rows(max_rows);
                extract_metadata_with_preview(path, &preview_opts)
            } else {
                extract_metadata(path)
            };

            match result {
                Ok(mut meta) => {
                    if let Some(algo) = &cli.checksum {
                        meta.checksum =
                            fmeta::compute_checksum(path, algo.clone()).ok();
                    }
                    if cli.statistics {
                        let stats_opts = fmeta::StatisticsOptions::new();
                        let _ = fmeta::compute_statistics(
                            path,
                            &mut meta,
                            &stats_opts,
                        );
                    }
                    if cli.classify {
                        let classify_opts = fmeta::ClassificationOptions::new();
                        let _ = fmeta::classify_columns(
                            path,
                            &mut meta,
                            &classify_opts,
                        );
                    }
                    output_meta(&meta, &cli);
                }
                Err(e) => {
                    eprintln!("error: {}: {e}", path.display());
                    exit_code = 1;
                }
            }
        }
    }

    std::process::exit(exit_code);
}

fn output_meta(meta: &fmeta::FileMetadata, cli: &Cli) {
    let json = if cli.pretty {
        serde_json::to_string_pretty(meta)
    } else {
        serde_json::to_string(meta)
    };
    match json {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error: failed to serialize metadata: {e}"),
    }
}
