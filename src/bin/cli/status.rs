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

    // Use server from .mekhan.json (ignore --server flag for push/status)
    let server_url = &meta.server_url;
    let template_id = &meta.template_id;

    println!("Comparing local with remote (template {})", template_id);

    // Connect and get remote state
    let handle = ws_client::connect_and_sync(server_url, template_id).await?;
    let (remote_graph, remote_files) = doc_ops::read_doc(&handle.doc)?;
    handle.disconnect().await?;

    // Compute diff (ignore positions for DSL formats)
    let local_format = formats::detect_format(&dir).unwrap_or(formats::WorkflowFormat::Json);
    let result = if local_format != formats::WorkflowFormat::Json {
        diff::compute_diff_ignoring_positions(&local_graph, &local_files, &remote_graph, &remote_files)
    } else {
        diff::compute_diff(&local_graph, &local_files, &remote_graph, &remote_files)
    };

    println!();
    result.print_summary();

    Ok(())
}
