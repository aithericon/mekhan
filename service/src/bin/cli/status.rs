use std::path::PathBuf;

use anyhow::Result;

use crate::diff;
use crate::doc_ops;
use crate::formats;
use crate::fs_ops;
use crate::ws_client;

pub async fn run(_server: &str, directory: &str) -> Result<()> {
    let dir = PathBuf::from(directory);

    // Import local state
    let (meta, local_graph, local_files) = fs_ops::import_from_dir(&dir)?;

    // Use server from the lock file (ignore --server flag for push/status)
    let server_url = &meta.server_url;
    let base_id = meta.require_base_template_id()?;

    // Resolve chain head — push/status target the latest row's Y.Doc, not
    // a stale historical version pinned in the lock.
    let latest = crate::http::resolve_latest(server_url, base_id).await?;
    let version_id = &latest.id;

    println!(
        "Comparing local with remote (template {} v{})",
        version_id, latest.version
    );

    // Connect and get remote state
    let handle = ws_client::connect_and_sync(server_url, version_id).await?;
    let (remote_graph, remote_files) = doc_ops::read_doc(&handle.doc)?;
    handle.disconnect().await?;

    // Compute diff (ignore positions for DSL formats)
    let local_format = formats::detect_format(&dir).unwrap_or(formats::WorkflowFormat::Json);
    let result = if local_format != formats::WorkflowFormat::Json {
        diff::compute_diff_ignoring_positions(
            &local_graph,
            &local_files,
            &remote_graph,
            &remote_files,
        )
    } else {
        diff::compute_diff(&local_graph, &local_files, &remote_graph, &remote_files)
    };

    println!();
    result.print_summary();

    Ok(())
}
