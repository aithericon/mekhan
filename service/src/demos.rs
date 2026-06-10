//! Built-in demo workflows, loaded from `demos/<name>/` directories on disk.
//!
//! Each demo lives under a top-level `demos/` directory containing:
//!
//! ```text
//! demos/<name>/
//!   demo.json                  # stable templateId + name + description
//!   graph.json                 # the WorkflowGraph (JSON)
//!   nodes/<id>/<file>          # per-node text source (e.g. main.py)
//!   nodes/<id>/task.json       # HumanTask form definition (overlay onto data.steps)
//! ```
//!
//! `nodes/<id>/task.json` is a node-metadata sidecar — a HumanTask is a
//! node like any other, so its form definition lives next to the
//! executable files of other node types (e.g. `nodes/extract/main.py`).
//! The loader merges the sidecar onto the matching HumanTask node's
//! `data.steps` and skips it from the regular text-file reader so it
//! doesn't double as a Y.Doc file.
//!
//! `demo.json` is the demo descriptor — a public, documented contract that
//! humans read (you read the templateId off it; you set the name +
//! description). The CLI's `mekhan.lock.json` is a separate, lockfile-style
//! bookkeeping artifact for pulled templates (server URL, last-pull
//! timestamp, format choice) — machine-managed and irrelevant to seeded
//! demos.
//!
//! Two halves:
//! - **Reader** ([`load_demo`], [`list_demo_dirs`]): turn a directory on
//!   disk into the `(metadata, graph, files)` triple a caller can hand
//!   to the `/api/v1/templates/.../apply` path. Used by tests.
//! - **Seeder** ([`seed_all`]): hand the loaded demos through the
//!   identical compile → upload → publish pipeline the `apply` handler
//!   uses, but bypass HTTP auth so the seeder can run at service startup
//!   before any user request. Idempotent by stable template id: if a row
//!   for the demo's id already exists, the seeder leaves it alone (user
//!   may have edited it).
//!
//! `graph.json` + `nodes/<id>/<file>` mirror the layout `cli::fs_ops`
//! writes for the GitOps `pull` flow — a demo directory is, modulo the
//! descriptor filename, identical to a pulled template. (CLI: `mekhan.lock.json`;
//! demo: `demo.json`.)
//!
//! Trigger-node id stability: the showcase used to mint a fresh trigger id
//! at every demo creation because the dispatcher registry is keyed
//! globally. With a single seeded demo per environment the id is fixed
//! (committed in `graph.json`); only when multiple parallel copies of the
//! demo are needed does the freshening become relevant again.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::models::template::WorkflowGraph;

/// `demo.json` shape — the public demo descriptor. Only the three fields
/// the seeder actually needs (stable id, name, description); the CLI's
/// per-checkout bookkeeping (server URL, last pull, format) lives in
/// `mekhan.lock.json` and is not modeled here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoMetadata {
    pub template_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Catalogue visibility for the seeded row. Absent ⇒ `public` — the
    /// historical default, so every existing demo keeps showing up in the
    /// root catalogue cross-workspace. Set `"private"` for a sub-workflow /
    /// agent-tool child (e.g. `08a`, `09b`) that should be hidden from the
    /// catalogue and embeddable only by its owning parent demo. A `private`
    /// demo MUST also declare `ownerTemplateId` (mirrors the
    /// `workflow_templates` CHECK that pairs `visibility='private'` with a
    /// non-null owner); `workspace`/`public` MUST NOT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Owning parent family base id — the `templateId` of the parent demo
    /// whose graph embeds this one as a SubWorkflow node. Required iff
    /// `visibility == "private"`. Seeded demos are born v1 with
    /// `base_template_id = id`, so the parent's family base IS its own
    /// `templateId` — paste it here verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_template_id: Option<String>,
    /// Category folder this demo is filed under, **relative to the `/demos`
    /// root**. A bare slug (`"streaming"`) nests one level (`/demos/streaming`);
    /// slashes nest deeper (`"robotics/xarm"` → `/demos/robotics/xarm`). Absent
    /// ⇒ filed directly in the `/demos` root. Known slugs (see
    /// [`DEMO_CATEGORIES`]) get a curated display name + blurb; unknown
    /// segments fall back to a title-cased slug. Ignored for `private` demos
    /// (they're hidden from the catalogue and never filed into a folder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// One parsed demo directory.
#[derive(Debug)]
pub struct LoadedDemo {
    pub metadata: DemoMetadata,
    pub graph: WorkflowGraph,
    /// `node_id → { filename → content }` — same shape every
    /// `/api/v1/templates` consumer expects.
    pub files: HashMap<String, HashMap<String, String>>,
    /// Pre-authored template tests bundled with the demo (one per
    /// `tests/<name>.json` sidecar). Empty when the demo carries no
    /// `tests/` directory. Seeded into `template_tests` alongside the
    /// template row, keyed by `name` for idempotency.
    pub tests: Vec<LoadedTest>,
}

/// A single bundled template test, parsed from `demos/<demo>/tests/<name>.json`.
/// Field shapes match `CreateTemplateTestRequest`; the seeder writes them
/// straight to `template_tests` as JSONB without re-validating (compile-time
/// validation is the user's problem when they author the file).
#[derive(Debug, serde::Deserialize)]
pub struct LoadedTest {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_empty_array")]
    pub start_tokens: serde_json::Value,
    #[serde(default = "default_empty_object")]
    pub human_answers: serde_json::Value,
    #[serde(default = "default_empty_array")]
    pub assertions: serde_json::Value,
}

fn default_true() -> bool {
    true
}
fn default_empty_array() -> serde_json::Value {
    serde_json::Value::Array(Vec::new())
}
fn default_empty_object() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

#[derive(Debug, Error)]
pub enum DemoLoadError {
    #[error("demo directory not found: {0}")]
    NotFound(PathBuf),
    #[error("metadata read failed at {path}: {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("metadata parse failed at {path}: {source}")]
    MetadataParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("graph read failed at {path}: {source}")]
    Graph {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("graph parse failed at {path}: {source}")]
    GraphParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("node file read failed at {path}: {source}")]
    NodeFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("task sidecar at {path} targets node `{node_id}` which is not in the graph")]
    TaskSidecarUnknownNode { path: PathBuf, node_id: String },
    #[error(
        "task sidecar at {path} targets a `{node_type}` node — task.json is only valid for human_task"
    )]
    TaskSidecarTypeMismatch { path: PathBuf, node_type: String },
    #[error("test sidecar read failed at {path}: {source}")]
    TestSidecar {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("test sidecar parse failed at {path}: {source}")]
    TestSidecarParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Load one demo directory. Skips files inside `nodes/<id>/` whose
/// extension marks them binary (mirrors the CLI's `is_binary_asset`
/// classification — binaries are template *assets*, not Y.Text files,
/// and the seeder uploads them through a different path).
pub fn load_demo(dir: &Path) -> Result<LoadedDemo, DemoLoadError> {
    if !dir.is_dir() {
        return Err(DemoLoadError::NotFound(dir.to_path_buf()));
    }

    let meta_path = dir.join("demo.json");
    let meta_str = std::fs::read_to_string(&meta_path).map_err(|e| DemoLoadError::Metadata {
        path: meta_path.clone(),
        source: e,
    })?;
    // `serde(deny_unknown_fields)` would help catch leftover CLI keys
    // (`serverUrl`, `lastPull`, `format`) but we keep it permissive so an
    // accidentally-pulled `mekhan.lock.json` renamed to `demo.json` still loads.
    let metadata: DemoMetadata =
        serde_json::from_str(&meta_str).map_err(|e| DemoLoadError::MetadataParse {
            path: meta_path.clone(),
            source: e,
        })?;

    let graph_path = dir.join("graph.json");
    let graph_str = std::fs::read_to_string(&graph_path).map_err(|e| DemoLoadError::Graph {
        path: graph_path.clone(),
        source: e,
    })?;
    let mut graph: WorkflowGraph =
        serde_json::from_str(&graph_str).map_err(|e| DemoLoadError::GraphParse {
            path: graph_path.clone(),
            source: e,
        })?;

    // Overlay HumanTask `data.steps` from `nodes/<id>/task.json` sidecars.
    // A HumanTask is a node like any other, so its form definition lives
    // under `nodes/<id>/` next to the executable files of other node
    // types (e.g. `nodes/extract/main.py`). The filename `task.json` is
    // the convention; `read_node_files` skips it so it doesn't double up
    // as a Y.Doc text file.
    let nodes_dir = dir.join("nodes");
    merge_task_sidecars(&nodes_dir, &mut graph)?;

    let files = read_node_files(&nodes_dir)?;

    let tests = read_tests_dir(&dir.join("tests"))?;

    Ok(LoadedDemo {
        metadata,
        graph,
        files,
        tests,
    })
}

/// Parse every `<name>.json` under `tests/` into a `LoadedTest`. Missing
/// directory is fine (most demos won't have one yet). Sorted by filename
/// so seed order is deterministic and `template_tests.created_at` reflects
/// the on-disk authoring order.
fn read_tests_dir(tests_dir: &Path) -> Result<Vec<LoadedTest>, DemoLoadError> {
    if !tests_dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(tests_dir).map_err(|e| DemoLoadError::TestSidecar {
        path: tests_dir.to_path_buf(),
        source: e,
    })?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::TestSidecar {
            path: tests_dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = std::fs::read(&path).map_err(|e| DemoLoadError::TestSidecar {
            path: path.clone(),
            source: e,
        })?;
        let test: LoadedTest =
            serde_json::from_slice(&bytes).map_err(|e| DemoLoadError::TestSidecarParse {
                path: path.clone(),
                source: e,
            })?;
        out.push(test);
    }
    Ok(out)
}

/// Filename used for HumanTask form-definition sidecars under `nodes/<id>/`.
/// Pulled out of the regular text-file reader (see [`TASK_SIDECAR_FILENAME`]
/// usage in [`read_node_files`]) so it doesn't double as a Y.Doc file.
const TASK_SIDECAR_FILENAME: &str = "task.json";

/// Walk `nodes/<id>/task.json` files and overlay each onto the matching
/// HumanTask node's `data.steps`. Missing sidecar leaves `data.steps` as
/// authored in graph.json (empty placeholder, which publish will reject —
/// a clear "you forgot the sidecar" failure mode). A sidecar for a node
/// that isn't a HumanTask (or doesn't exist) is a hard error here so
/// typos surface at load time, not at publish.
fn merge_task_sidecars(nodes_dir: &Path, graph: &mut WorkflowGraph) -> Result<(), DemoLoadError> {
    use crate::models::template::{TaskStepConfig, WorkflowNodeData};

    if !nodes_dir.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(nodes_dir).map_err(|e| DemoLoadError::NodeFile {
        path: nodes_dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::NodeFile {
            path: nodes_dir.to_path_buf(),
            source: e,
        })?;
        let ft = entry.file_type().map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        if !ft.is_dir() {
            continue;
        }
        let sidecar = entry.path().join(TASK_SIDECAR_FILENAME);
        if !sidecar.is_file() {
            continue;
        }
        let node_id = entry.file_name().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&sidecar).map_err(|e| DemoLoadError::NodeFile {
            path: sidecar.clone(),
            source: e,
        })?;
        let steps: Vec<TaskStepConfig> =
            serde_json::from_str(&text).map_err(|e| DemoLoadError::GraphParse {
                path: sidecar.clone(),
                source: e,
            })?;

        let node = graph.nodes.iter_mut().find(|n| n.id == node_id);
        match node {
            Some(n) => match &mut n.data {
                WorkflowNodeData::HumanTask { steps: target, .. } => {
                    *target = steps;
                }
                _ => {
                    return Err(DemoLoadError::TaskSidecarTypeMismatch {
                        path: sidecar,
                        node_type: n.data.type_name().to_string(),
                    });
                }
            },
            None => {
                return Err(DemoLoadError::TaskSidecarUnknownNode {
                    path: sidecar,
                    node_id,
                });
            }
        }
    }
    Ok(())
}

/// Read every text file under `nodes/<id>/`. Empty result if the directory
/// is absent — a demo with no scripted nodes is legal.
fn read_node_files(
    nodes_dir: &Path,
) -> Result<HashMap<String, HashMap<String, String>>, DemoLoadError> {
    let mut files: HashMap<String, HashMap<String, String>> = HashMap::new();
    if !nodes_dir.is_dir() {
        return Ok(files);
    }

    let entries = std::fs::read_dir(nodes_dir).map_err(|e| DemoLoadError::NodeFile {
        path: nodes_dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::NodeFile {
            path: nodes_dir.to_path_buf(),
            source: e,
        })?;
        let file_type = entry.file_type().map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        if !file_type.is_dir() {
            continue;
        }

        let node_id = entry.file_name().to_string_lossy().into_owned();
        let mut node_files: HashMap<String, String> = HashMap::new();

        let inner = std::fs::read_dir(entry.path()).map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        for file_entry in inner {
            let file_entry = file_entry.map_err(|e| DemoLoadError::NodeFile {
                path: entry.path(),
                source: e,
            })?;
            let ft = file_entry
                .file_type()
                .map_err(|e| DemoLoadError::NodeFile {
                    path: file_entry.path(),
                    source: e,
                })?;
            if !ft.is_file() {
                continue;
            }
            let filename = file_entry.file_name().to_string_lossy().into_owned();
            if is_binary_asset(&filename) {
                continue;
            }
            // `task.json` is HumanTask form-definition metadata, not a
            // Y.Doc text file — `merge_task_sidecars` consumes it
            // separately. Skip it here so it doesn't ship twice.
            if filename == TASK_SIDECAR_FILENAME {
                continue;
            }
            let content = std::fs::read_to_string(file_entry.path()).map_err(|e| {
                DemoLoadError::NodeFile {
                    path: file_entry.path(),
                    source: e,
                }
            })?;
            node_files.insert(filename, content);
        }

        if !node_files.is_empty() {
            files.insert(node_id, node_files);
        }
    }
    Ok(files)
}

/// Filename extensions classified as binary assets — skipped by the text-file
/// reader so they go through the asset-upload path instead. Same list the
/// CLI uses; kept duplicated rather than cross-crate-imported so the
/// service library doesn't depend on the binary crate.
fn is_binary_asset(filename: &str) -> bool {
    const BINARY_EXTENSIONS: &[&str] = &[
        "png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp", "tiff", "pdf", "zip", "tar",
        "gz", "whl",
    ];
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    BINARY_EXTENSIONS.contains(&ext.as_str())
}

/// Enumerate the demo subdirectories of a `demos/` root, in stable lexical
/// order. Returns the directory paths; callers feed each to `load_demo`.
pub fn list_demo_dirs(root: &Path) -> Result<Vec<PathBuf>, DemoLoadError> {
    if !root.is_dir() {
        return Err(DemoLoadError::NotFound(root.to_path_buf()));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|e| DemoLoadError::NodeFile {
        path: root.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| DemoLoadError::NodeFile {
            path: root.to_path_buf(),
            source: e,
        })?;
        let ft = entry.file_type().map_err(|e| DemoLoadError::NodeFile {
            path: entry.path(),
            source: e,
        })?;
        if !ft.is_dir() {
            continue;
        }
        if entry.path().join("demo.json").is_file() {
            out.push(entry.path());
        }
    }
    out.sort();
    Ok(out)
}

// ── Seeder ──────────────────────────────────────────────────────────────────

/// Errors the startup seeder can surface to the caller. Each variant carries
/// enough context to log a single actionable line — the seeder is
/// best-effort by design (a failure to seed the demo must not prevent the
/// service from starting) so the binary logs and continues.
#[derive(Debug, Error)]
pub enum DemoSeedError {
    #[error("load demo failed: {0}")]
    Load(#[from] DemoLoadError),
    #[error("metadata templateId `{0}` is not a valid UUID")]
    InvalidTemplateId(String),
    #[error("metadata ownerTemplateId `{0}` is not a valid UUID")]
    InvalidOwnerTemplateId(String),
    #[error("visibility `{0}` is invalid — must be `workspace`, `public`, or `private`")]
    InvalidVisibility(String),
    #[error(
        "visibility `private` requires `ownerTemplateId` (the embedding parent demo's templateId)"
    )]
    PrivateMissingOwner,
    #[error("`ownerTemplateId` is only valid with `visibility: private`")]
    OwnerOnNonPrivate,
    #[error("db error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("compile failed: {0}")]
    Compile(String),
    #[error("s3 upload failed: {0}")]
    Upload(String),
    #[error("yjs init failed: {0}")]
    Yjs(String),
}

/// One-shot outcome of seeding a single demo. The seeder logs and
/// continues; the binary calls this in a loop and totals the actions.
#[derive(Debug, Clone, Copy)]
pub enum SeedOutcome {
    /// Template already existed (matched by stable id). Left untouched —
    /// the user may have edited it through the web editor.
    AlreadyPresent,
    /// Row + AIR + S3 files + Y.Doc + triggers freshly created.
    Seeded,
}

/// Synthetic actor id used for the `author_id` column on seeded templates.
/// Same value across all environments so a `SELECT * WHERE author_id = X`
/// reliably distinguishes seeded demos from user-authored content.
///
/// `00000000-0000-0000-0000-000000000aaa` — chosen to be obviously
/// non-Zitadel (real user subjects are random v4 UUIDs) and to sort
/// distinctly from the nil UUID some test fixtures use.
const DEMO_SEEDER_AUTHOR_ID: uuid::Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000aaa");

/// Workspace that seeded demos belong to: the **default** workspace
/// (`Uuid::nil()`, slug `default`), NOT the system-owned `demos` workspace.
///
/// Demos are meant to be first-class, *editable* starting points — the
/// dev-noop user owns `default` (migration 20240123) and the BFF resolver
/// auto-provisions every authenticated user as an `editor` of it
/// (`ensure_default_workspace_membership`). So seeding here means a user can
/// open a demo and publish edits without hitting `gate_template_write`'s
/// membership check — which is exactly what a separate system-owned demos
/// workspace prevented. Rows are still `visibility = 'public'` so users whose
/// active workspace is some *other* tenant still discover them via the
/// cross-workspace public-read branch in `list_templates`.
const DEMO_WORKSPACE_ID: uuid::Uuid = uuid::Uuid::nil();

/// Slug of the root folder every seeded demo lives under. Folders are a
/// non-ACL grouping inside a workspace (see migration 20240149), so this is
/// purely organizational: it keeps the built-in demos collected under one
/// `/demos` tree in the default workspace instead of scattered among user
/// templates. The root-slug partial unique index makes the slug the
/// idempotency key.
const DEMO_ROOT_SLUG: &str = "demos";

/// One curated demo category folder: its path-segment `slug` (the
/// `(workspace_id, parent_id, slug)` idempotency key), display name, and blurb.
/// Everything but `slug` is cosmetic catalogue copy.
struct DemoCategory {
    slug: &'static str,
    display_name: &'static str,
    description: &'static str,
}

/// The built-in demo category folders, each a child of the `/demos` root. A
/// demo names one by slug via `demo.json::folder`; an absent slug files the
/// demo directly in the root, and an unknown slug still gets a folder (with a
/// title-cased display name). Purely organizational — it groups related demos
/// under one heading instead of one flat `/demos` list.
const DEMO_CATEGORIES: &[DemoCategory] = &[
    DemoCategory {
        slug: "basics",
        display_name: "Basics",
        description: "The core primitives, in learning-path order: Start/Automated/End, \
                      human tasks, decisions, loops, parallel fan-out, sub-workflows, forms.",
    },
    DemoCategory {
        slug: "control-flow",
        display_name: "Control Flow",
        description: "Failure handling (the error port, retries, wired vs. unwired \
                      failures) and timing (delay / timeout).",
    },
    DemoCategory {
        slug: "agents-llm",
        display_name: "Agents & LLM",
        description: "LLM-driven steps: OCR→classify→extract, multi-turn agents with \
                      sub-workflow tools, and self-hosted model-pool inference.",
    },
    DemoCategory {
        slug: "integrations",
        display_name: "Integrations",
        description: "Executor backends that talk to external systems: HTTP, Postgres, \
                      Loki/LogQL, Prometheus/PromQL.",
    },
    DemoCategory {
        slug: "optimization",
        display_name: "Optimization",
        description: "Bayesian-optimization loops: catalog triggers, observation \
                      producers, and the optimize/observe cycle.",
    },
    DemoCategory {
        slug: "pools",
        display_name: "Pools & Leasing",
        description: "Capacity primitives: resource pools, leased GPUs, runner pools, \
                      and runner cross-resource declarations.",
    },
    DemoCategory {
        slug: "streaming",
        display_name: "Streaming",
        description: "Channel streaming end to end: control/data planes, stream map / \
                      pipeline, audio transcription, and live audio/video media streams.",
    },
    DemoCategory {
        slug: "assets",
        display_name: "Assets & Resources",
        description: "Typed assets and workspace resources: consuming, referencing, \
                      and reading them from Python.",
    },
    DemoCategory {
        slug: "robotics",
        display_name: "Robotics",
        description: "ROS-driven fleets through the platform: turtlesim, the xArm 6 \
                      (joint / trajectory / scene / grasp), and sample-handling cells.",
    },
    DemoCategory {
        slug: "online-clinic",
        display_name: "Online Clinic",
        description: "Online-clinic document-intake workflows (OCR → classify → \
                      extract → safety gate) ported onto mekhan primitives.",
    },
    DemoCategory {
        slug: "examples",
        display_name: "Examples",
        description: "End-to-end example workflows that tie the primitives together.",
    },
];

/// Curated metadata for a category path segment, if it is a known slug.
fn category_meta(slug: &str) -> Option<&'static DemoCategory> {
    DEMO_CATEGORIES.iter().find(|c| c.slug == slug)
}

/// Title-case a kebab slug for the display name of an unknown category segment
/// (`"my-folder"` → `"My Folder"`).
fn title_from_slug(slug: &str) -> String {
    slug.split('-')
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Upsert the `/demos` root folder in the default workspace and return its id.
/// Keyed on the root-slug partial unique index (`folders_root_slug_uniq`);
/// `ON CONFLICT DO UPDATE` keeps the copy fresh and guarantees a `RETURNING id`
/// even on the conflict path.
async fn ensure_demo_root(state: &crate::AppState) -> Result<uuid::Uuid, DemoSeedError> {
    let (id,): (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO folders (workspace_id, parent_id, slug, display_name, description, path, created_by) \
              VALUES ($1, NULL, $2, $3, $4, $5, $6) \
         ON CONFLICT (workspace_id, slug) WHERE parent_id IS NULL \
              DO UPDATE SET display_name = EXCLUDED.display_name, \
                            description = EXCLUDED.description \
         RETURNING id",
    )
    .bind(DEMO_WORKSPACE_ID)
    .bind(DEMO_ROOT_SLUG)
    .bind("Demos")
    .bind("Built-in example workflows seeded by mekhan-service.")
    .bind(format!("/{DEMO_ROOT_SLUG}"))
    .bind(DEMO_SEEDER_AUTHOR_ID)
    .fetch_one(&state.db)
    .await?;
    Ok(id)
}

/// Ensure the category folder chain for a demo exists under `/demos` and return
/// the **leaf** folder id the demo should be filed into. `folder` is the
/// `demo.json::folder` path relative to the root (`"streaming"`,
/// `"robotics/xarm"`); `None`/empty files the demo directly in the root.
///
/// Each path segment is upserted as a child of the previous one, keyed on
/// `(workspace_id, parent_id, slug)` so reseeding is idempotent. Known segments
/// (see [`DEMO_CATEGORIES`]) carry curated copy; unknown ones get a title-cased
/// display name and an empty blurb.
async fn ensure_demo_folder(
    state: &crate::AppState,
    folder: Option<&str>,
) -> Result<uuid::Uuid, DemoSeedError> {
    let mut parent = ensure_demo_root(state).await?;
    let Some(path) = folder else {
        return Ok(parent);
    };
    let mut acc = format!("/{DEMO_ROOT_SLUG}");
    for seg in path.split('/').map(str::trim).filter(|s| !s.is_empty()) {
        acc.push('/');
        acc.push_str(seg);
        let (display_name, description) = match category_meta(seg) {
            Some(c) => (c.display_name.to_string(), c.description.to_string()),
            None => (title_from_slug(seg), String::new()),
        };
        let (id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO folders (workspace_id, parent_id, slug, display_name, description, path, created_by) \
                  VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (workspace_id, parent_id, slug) \
                  DO UPDATE SET display_name = EXCLUDED.display_name, \
                                description = EXCLUDED.description, \
                                path = EXCLUDED.path \
             RETURNING id",
        )
        .bind(DEMO_WORKSPACE_ID)
        .bind(parent)
        .bind(seg)
        .bind(&display_name)
        .bind(&description)
        .bind(&acc)
        .bind(DEMO_SEEDER_AUTHOR_ID)
        .fetch_one(&state.db)
        .await?;
        parent = id;
    }
    Ok(parent)
}

/// Seed every demo under `root` into the running service. Idempotent:
/// each demo's `demo.json::templateId` is the stable identifier — if
/// a row with that id already exists, the seeder leaves it (logging
/// "already present") regardless of content drift.
///
/// Logs and continues on per-demo failure; only a totally missing `root`
/// or a non-recoverable DB / S3 error surfaces. The caller (service main)
/// treats the return value as advisory: the demo not being seeded must
/// not prevent the service from accepting requests.
/// Make the demo seeder principal an `owner` of the demo workspace. The
/// publish-time resource resolver gates reads on `workspace_members`
/// membership (the `resource_acl` table is auto-granted on create but not
/// consulted on the read path), so without this the seeder — publishing as
/// [`DEMO_SEEDER_AUTHOR_ID`], which never flows through the BFF
/// `ensure_default_workspace_membership` path that real users do — cannot
/// resolve any workspace resource a demo references. Idempotent.
async fn ensure_seeder_workspace_membership(state: &crate::AppState) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
         VALUES ($1, $2, 'owner') \
         ON CONFLICT (workspace_id, user_id) DO NOTHING",
    )
    .bind(DEMO_WORKSPACE_ID)
    .bind(DEMO_SEEDER_AUTHOR_ID)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Provision the capability types a presence-pooled demo's placement
/// Requirements reference, from `<root>/capability-types/*.json`. Each file is
/// a [`crate::models::capability::CreateCapabilityTypeRequest`] (name + typed
/// fields); the workspace is forced to the demo workspace and the row is
/// inserted as the seeder principal — the same DB shape the cookie-gated
/// `POST /api/v1/capability-types` handler writes, minus the HTTP boundary.
/// Idempotent: a same-named LIVE type is left as-is (never re-inserted).
///
/// Runs BEFORE the demo loop so that (a) a presence-pooled step whose
/// `requirements` name a capability passes `validate_requirements_against_registry`
/// at publish time, and (b) a runner enrolling with `--capabilities '{"<cap>":…}'`
/// passes enroll-time `validate_caps_against_types`. Best-effort: a fixture
/// failure is logged, never fatal — the dependent demo simply won't seed.
async fn seed_demo_capability_types(state: &crate::AppState, root: &Path) {
    use crate::models::capability::CreateCapabilityTypeRequest;
    let dir = root.join("capability-types");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return, // no fixtures directory — nothing to provision
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo capability-type: read failed");
                continue;
            }
        };
        let req: CreateCapabilityTypeRequest = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo capability-type: parse failed");
                continue;
            }
        };
        let name = req.name.trim().to_string();
        let existing: Result<Option<(uuid::Uuid,)>, _> = sqlx::query_as(
            "SELECT id FROM capability_types \
             WHERE workspace_id = $1 AND name = $2 AND revoked_at IS NULL",
        )
        .bind(DEMO_WORKSPACE_ID)
        .bind(&name)
        .fetch_optional(&state.db)
        .await;
        match existing {
            Ok(Some(_)) => {
                tracing::info!(capability_type = %name, "demo capability-type already present — left as-is");
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(capability_type = %name, error = %e, "demo capability-type: existence check failed");
                continue;
            }
        }
        let fields_json = match serde_json::to_value(&req.fields) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(capability_type = %name, error = %e, "demo capability-type: serialize fields failed");
                continue;
            }
        };
        let insert = sqlx::query(
            "INSERT INTO capability_types (id, workspace_id, name, fields, created_by) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(DEMO_WORKSPACE_ID)
        .bind(&name)
        .bind(&fields_json)
        .bind(DEMO_SEEDER_AUTHOR_ID)
        .execute(&state.db)
        .await;
        match insert {
            Ok(_) => tracing::info!(capability_type = %name, "demo capability-type seeded"),
            Err(e) => {
                tracing::warn!(capability_type = %name, "demo capability-type seed failed: {e:?}")
            }
        }
    }
}

/// Provision the resource fixtures demos bind, from `<root>/resources/*.json`.
/// Substitute `${VAR}` and `${VAR:-default}` in a fixture string from the
/// seeder process env. Lets a demo resource point at a slot-varying dev
/// endpoint (the inference router's port differs per worktree slot) without
/// hardcoding it; an unset var with no `:-default` collapses to empty. Seed-time
/// only (once per boot), so the per-call regex compile is irrelevant.
fn interpolate_env(raw: &str) -> String {
    let re = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}")
        .expect("static env-interpolation regex is valid");
    re.replace_all(raw, |caps: &regex::Captures| match std::env::var(&caps[1]) {
        Ok(v) => v,
        Err(_) => caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default(),
    })
    .into_owned()
}

/// Each file is a [`CreateResourceRequest`] (path + resource_type + config);
/// the workspace is forced to the demo workspace and the resource is created
/// as the seeder principal (Vault secret + ACL + audit, identical to the HTTP
/// CRUD path). Idempotent: a resource whose `path` already exists in the
/// workspace is left as-is — mirrors the template seeder, never clobbers a
/// user-edited resource or rewrites Vault every boot.
///
/// Runs BEFORE the demo loop so a demo that binds a workspace resource (e.g.
/// `email-welcome`'s `send` step → the `mail` SMTP relay) can resolve it at
/// publish time. Best-effort: a fixture failure is logged, never fatal — the
/// dependent demo simply won't seed, like any other compile failure.
async fn seed_demo_resources(state: &crate::AppState, root: &Path) {
    use crate::models::resource::CreateResourceRequest;
    let dir = root.join("resources");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return, // no fixtures directory — nothing to provision
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo resource: read failed");
                continue;
            }
        };
        // Interpolate `${VAR}` / `${VAR:-default}` against the seeder process env
        // BEFORE parsing, so a fixture can point at a slot-varying endpoint
        // (e.g. `internal_pool_router.base_url = ${MEKHAN_ROUTER_URL:-…}` → the
        // live inference-router for this dev slot) instead of a hardcoded port.
        let raw = interpolate_env(&raw);
        let req: CreateResourceRequest = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo resource: parse failed");
                continue;
            }
        };
        let existing: Result<Option<(uuid::Uuid,)>, _> = sqlx::query_as(
            "SELECT id FROM resources \
             WHERE workspace_id = $1 AND path = $2 AND deleted_at IS NULL",
        )
        .bind(DEMO_WORKSPACE_ID)
        .bind(&req.path)
        .fetch_optional(&state.db)
        .await;
        match existing {
            Ok(Some(_)) => {
                // DB row survives a `just dev down/up`, but the in-memory dev
                // Vault does not — re-assert the fixture's secret so a bound
                // demo doesn't hit `secret not found …#password` at fire time.
                // The DB row (config/version/ACL) is left untouched.
                match crate::handlers::resources::reprovision_resource_secret(
                    state,
                    &req,
                    DEMO_WORKSPACE_ID,
                )
                .await
                {
                    Ok(()) => tracing::info!(
                        resource = %req.path,
                        "demo resource already present — secret re-provisioned, config left as-is"
                    ),
                    Err(e) => tracing::warn!(
                        resource = %req.path,
                        "demo resource present but secret re-provision failed: {e:?}"
                    ),
                }
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(resource = %req.path, error = %e, "demo resource: existence check failed");
                continue;
            }
        }
        match crate::handlers::resources::create_resource_internal(
            state,
            &req,
            DEMO_WORKSPACE_ID,
            DEMO_SEEDER_AUTHOR_ID,
        )
        .await
        {
            Ok(s) => {
                tracing::info!(resource = %s.path, resource_type = %s.resource_type, "demo resource seeded")
            }
            Err(e) => {
                tracing::warn!(resource = %req.path, "demo resource seed failed: {e:?}")
            }
        }
    }
}

/// One `demos/roster/*.json` enrollment fixture: enrols a (dev) member into a
/// seeded `human`-preset `capacity` so a HumanTask-on-offer demo (docs/33) has
/// an eligible roster the moment it boots. `capacity` is the workspace path of
/// the capacity resource the fixture's task binds (resolved to its row id);
/// `member_user_id` is the enrolee (in `dev_noop` the fixed dev user). `caps`
/// are advertised capabilities matched against a task's `requirements` by the
/// offer pool's `t_claim` guard; `availability`/`available` mirror the durable
/// `roster_members` knobs (a `none` liveness source = admitted on the toggle
/// alone, no heartbeat — see [`crate::models::roster::LivenessSource`]).
#[derive(serde::Deserialize)]
struct RosterFixture {
    /// Workspace `path` of the `capacity` resource to enrol into.
    capacity: String,
    member_user_id: uuid::Uuid,
    #[serde(default = "empty_caps")]
    caps: serde_json::Value,
    #[serde(default = "one_concurrency")]
    concurrency: i32,
    #[serde(default = "empty_caps")]
    availability: serde_json::Value,
    #[serde(default)]
    available: bool,
}

fn empty_caps() -> serde_json::Value {
    serde_json::json!({})
}
fn one_concurrency() -> i32 {
    1
}

/// Seed human-capacity roster enrolments from `demos/roster/*.json` into the
/// demo workspace. Mirrors [`seed_demo_resources`]: runs (from [`seed_all`])
/// AFTER it, so each fixture's `capacity` path resolves to a real row. The
/// enrolment is idempotent on the `(workspace_id, capacity_id, member_user_id)`
/// unique key (`DO NOTHING` — a hand-toggled `available` survives reboots).
/// Best-effort: a missing capacity or insert error is logged, never fatal.
async fn seed_demo_roster(state: &crate::AppState, root: &Path) {
    let dir = root.join("roster");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return, // no roster fixtures — nothing to enrol
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo roster: read failed");
                continue;
            }
        };
        let fx: RosterFixture = match serde_json::from_str(&raw) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo roster: parse failed");
                continue;
            }
        };
        let capacity_id: Result<Option<(uuid::Uuid,)>, _> = sqlx::query_as(
            "SELECT id FROM resources \
             WHERE workspace_id = $1 AND path = $2 \
               AND resource_type = 'capacity' AND deleted_at IS NULL",
        )
        .bind(DEMO_WORKSPACE_ID)
        .bind(&fx.capacity)
        .fetch_optional(&state.db)
        .await;
        let capacity_id = match capacity_id {
            Ok(Some((id,))) => id,
            Ok(None) => {
                tracing::warn!(
                    capacity = %fx.capacity,
                    "demo roster: capacity not found — skipping enrolment (is its resource fixture seeded?)"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(capacity = %fx.capacity, error = %e, "demo roster: capacity lookup failed");
                continue;
            }
        };
        let res = sqlx::query(
            "INSERT INTO roster_members \
             (workspace_id, capacity_id, member_user_id, caps, concurrency, availability, available, enrolled_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (workspace_id, capacity_id, member_user_id) DO NOTHING",
        )
        .bind(DEMO_WORKSPACE_ID)
        .bind(capacity_id)
        .bind(fx.member_user_id)
        .bind(&fx.caps)
        .bind(fx.concurrency)
        .bind(&fx.availability)
        .bind(fx.available)
        .bind(DEMO_SEEDER_AUTHOR_ID)
        .execute(&state.db)
        .await;
        match res {
            Ok(_) => tracing::info!(
                capacity = %fx.capacity,
                member = %fx.member_user_id,
                "demo roster member enrolled"
            ),
            Err(e) => {
                tracing::warn!(capacity = %fx.capacity, error = %e, "demo roster enrol failed")
            }
        }
    }
}

/// One `model_states` seed fixture (`demos/model_states/*.json`): pins a
/// curated model into the loaded-state machine so `GET /api/v1/models` reports
/// it `loaded` after a fresh seed (model-pool P1, docs/29 §3). `state` is the
/// operator-curated lifecycle position; `registry_resource` (optional) names
/// the `model_registry` resource this model belongs to — resolved to its
/// `resources.id` at seed time, best-effort.
#[derive(serde::Deserialize)]
struct ModelStateFixture {
    model_id: String,
    /// Free-text lifecycle position; validated against [`crate::models::model_pool::ModelState`].
    state: String,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    replicas: i32,
    #[serde(default)]
    note: Option<String>,
    /// Optional `path` of the `model_registry` resource backing this model.
    #[serde(default)]
    registry_resource: Option<String>,
}

/// Seed the `model_states` projection rows that the model-pool demos rely on
/// (model-pool P1, docs/29 §3), from `demos/model_states/*.json`, into the demo
/// workspace. Mirrors [`seed_demo_resources`]: runs BEFORE the demo loop so a
/// model-pool demo's loaded model id is curated by the time its Agent step
/// compiles. Idempotent via `ON CONFLICT (workspace_id, model_id) DO UPDATE` —
/// re-asserts the fixture's state on every boot (the in-memory dev stack is
/// reset on `just dev reset`, but the row otherwise survives; re-asserting keeps
/// the fixture the source of truth, same spirit as the resource-secret
/// reprovision). The seeded `state` is NOT re-validated through the Rust state
/// machine (that gates operator-driven `POST .../transition` edges, not the
/// seed); a fixture with an unknown `state` string fails CLOSED to `unloaded`
/// at read time (`ModelStateRow::into_view`). Best-effort: a fixture failure is
/// logged, never fatal.
async fn seed_model_states(state: &crate::AppState, root: &Path) {
    let dir = root.join("model_states");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return, // no fixtures directory — nothing to seed
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "model_states fixture: read failed");
                continue;
            }
        };
        let fx: ModelStateFixture = match serde_json::from_str(&raw) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "model_states fixture: parse failed");
                continue;
            }
        };
        // Resolve the optional backing registry resource → its row id (the
        // projection's `registry_resource_id`). Absent / unresolved → NULL.
        let registry_resource_id: Option<uuid::Uuid> = match &fx.registry_resource {
            Some(alias) => {
                let row: Result<Option<(uuid::Uuid,)>, _> = sqlx::query_as(
                    "SELECT id FROM resources \
                     WHERE workspace_id = $1 AND path = $2 AND deleted_at IS NULL",
                )
                .bind(DEMO_WORKSPACE_ID)
                .bind(alias)
                .fetch_optional(&state.db)
                .await;
                match row {
                    Ok(r) => r.map(|(id,)| id),
                    Err(e) => {
                        tracing::warn!(model_id = %fx.model_id, registry = %alias, error = %e, "model_states fixture: registry lookup failed");
                        None
                    }
                }
            }
            None => None,
        };
        let res = sqlx::query(
            "INSERT INTO model_states \
                 (workspace_id, registry_resource_id, model_id, state, base, replicas, note) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (workspace_id, model_id) DO UPDATE SET \
                 registry_resource_id = EXCLUDED.registry_resource_id, \
                 state = EXCLUDED.state, \
                 base = EXCLUDED.base, \
                 replicas = EXCLUDED.replicas, \
                 note = EXCLUDED.note, \
                 last_transition_at = NOW()",
        )
        .bind(DEMO_WORKSPACE_ID)
        .bind(registry_resource_id)
        .bind(&fx.model_id)
        .bind(&fx.state)
        .bind(&fx.base)
        .bind(fx.replicas)
        .bind(&fx.note)
        .execute(&state.db)
        .await;
        match res {
            Ok(_) => {
                tracing::info!(model_id = %fx.model_id, state = %fx.state, "model_states fixture seeded")
            }
            Err(e) => {
                tracing::warn!(model_id = %fx.model_id, "model_states fixture seed failed: {e:?}")
            }
        }
    }
}

/// Best-effort content-type from a bundled file's extension, for the asset
/// File-field seed upload. Falls back to `application/octet-stream`.
fn guess_content_type(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("txt") => "text/plain",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

/// One asset fixture: a self-contained asset-type schema + the asset (ref-key)
/// + its records. The type is created (or reused if a same-named type already
///   exists in the demo workspace) before the asset, so several fixtures can
///   share a type without ordering ceremony. A record's `File`-field value may be
///   `{"__file": "<path-relative-to-demos/assets>"}` — the seeder uploads that
///   bundled file and substitutes the resulting storage path.
#[derive(serde::Deserialize)]
struct AssetFixture {
    asset_type: crate::models::asset::CreateAssetTypeRequest,
    ref_key: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    records: Vec<serde_json::Value>,
}

/// Provision the curated assets that demos reference (docs/20), from
/// `demos/assets/*.json`, into the demo workspace. Mirrors
/// [`seed_demo_resources`]: runs BEFORE the demo loop so a demo that binds /
/// reads an asset (e.g. `21-asset-consume`'s `metals_db`, `22-asset-ref`'s
/// `steel_spec`) can resolve it at publish time. Idempotent — an asset whose
/// ref-key already exists is left as-is (records are NOT rewritten, so a
/// developer's live edits survive a reseed). Best-effort: a fixture failure is
/// logged, never fatal — the dependent demo simply won't seed.
async fn seed_demo_assets(state: &crate::AppState, root: &Path) {
    use crate::models::asset::{CreateAssetRequest, ScopeKind};

    let dir = root.join("assets");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return, // no fixtures directory — nothing to provision
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    paths.sort();

    let scope_kind = ScopeKind::Workspace;
    let scope_id = DEMO_WORKSPACE_ID;

    for path in paths {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo asset: read failed");
                continue;
            }
        };
        let fixture: AssetFixture = match serde_json::from_str(&raw) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(fixture = %path.display(), error = %e, "demo asset: parse failed");
                continue;
            }
        };

        // Resolve (or create) the asset type by name within the demo workspace.
        let type_name = fixture.asset_type.name.clone();
        let existing_type: Result<Option<(uuid::Uuid,)>, _> = sqlx::query_as(
            "SELECT id FROM asset_types \
             WHERE scope_kind = $1 AND scope_id = $2 AND name = $3 AND deleted_at IS NULL",
        )
        .bind(scope_kind.as_db())
        .bind(scope_id)
        .bind(&type_name)
        .fetch_optional(&state.db)
        .await;
        let type_id = match existing_type {
            Ok(Some((id,))) => id,
            Ok(None) => {
                match crate::handlers::assets::create_asset_type_internal(
                    state,
                    &fixture.asset_type,
                    scope_kind,
                    scope_id,
                    DEMO_SEEDER_AUTHOR_ID,
                )
                .await
                {
                    Ok(detail) => {
                        tracing::info!(asset_type = %type_name, "demo asset type seeded");
                        detail.id
                    }
                    Err(e) => {
                        tracing::warn!(asset_type = %type_name, "demo asset type seed failed: {e:?}");
                        continue;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(asset_type = %type_name, error = %e, "demo asset: type lookup failed");
                continue;
            }
        };

        // Idempotent: an existing asset (by ref-key) is left untouched.
        let existing_asset: Result<Option<(uuid::Uuid,)>, _> = sqlx::query_as(
            "SELECT id FROM assets \
             WHERE scope_kind = $1 AND scope_id = $2 AND ref_key = $3 AND deleted_at IS NULL",
        )
        .bind(scope_kind.as_db())
        .bind(scope_id)
        .bind(&fixture.ref_key)
        .fetch_optional(&state.db)
        .await;
        match existing_asset {
            Ok(Some(_)) => {
                tracing::info!(asset = %fixture.ref_key, "demo asset already present — leaving as-is");
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(asset = %fixture.ref_key, error = %e, "demo asset: existence check failed");
                continue;
            }
        }

        let create = CreateAssetRequest {
            type_id,
            ref_key: fixture.ref_key.clone(),
            display_name: fixture.display_name.clone(),
            display_path: None,
            scope_kind: None,
            scope_id: None,
        };
        let asset = match crate::handlers::assets::create_asset_internal(
            state,
            &create,
            scope_kind,
            scope_id,
            DEMO_SEEDER_AUTHOR_ID,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(asset = %fixture.ref_key, "demo asset seed failed: {e:?}");
                continue;
            }
        };

        if !fixture.records.is_empty() {
            // Resolve `File`-field `{"__file": "<rel>"}` directives: upload the
            // bundled file (relative to `demos/assets/`) and substitute the
            // storage path, so a seeded asset carries real File-field content
            // (demo 24 then always exercises `File.retrieve()`). Collected
            // first, then uploaded, to avoid borrowing the record map across
            // the await.
            let mut records = fixture.records.clone();
            let mut directives: Vec<(usize, String, String)> = Vec::new();
            for (i, rec) in records.iter().enumerate() {
                if let Some(obj) = rec.as_object() {
                    for (field, val) in obj {
                        if let Some(rel) = val
                            .as_object()
                            .and_then(|o| o.get("__file"))
                            .and_then(|v| v.as_str())
                        {
                            directives.push((i, field.clone(), rel.to_string()));
                        }
                    }
                }
            }
            let mut upload_ok = true;
            for (i, field, rel) in directives {
                let file_path = dir.join(&rel);
                let bytes = match std::fs::read(&file_path) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(asset = %fixture.ref_key, file = %file_path.display(), error = %e, "demo asset: bundled File read failed");
                        upload_ok = false;
                        break;
                    }
                };
                let filename = file_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("upload.bin")
                    .to_string();
                match state
                    .s3
                    .upload_asset_file(
                        asset.id,
                        asset.version,
                        &field,
                        &filename,
                        &bytes,
                        guess_content_type(&filename),
                    )
                    .await
                {
                    Ok(key) => {
                        if let Some(o) = records[i].as_object_mut() {
                            o.insert(field, serde_json::Value::String(key));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(asset = %fixture.ref_key, %field, "demo asset: File upload failed: {e:?}");
                        upload_ok = false;
                        break;
                    }
                }
            }
            if !upload_ok {
                continue;
            }
            if let Err(e) =
                crate::handlers::assets::replace_records_internal(state, asset.id, &records).await
            {
                tracing::warn!(asset = %fixture.ref_key, "demo asset records seed failed: {e:?}");
                continue;
            }
        }
        tracing::info!(asset = %fixture.ref_key, rows = fixture.records.len(), "demo asset seeded");
    }
}

pub async fn seed_all(
    state: &crate::AppState,
    root: &Path,
) -> Result<Vec<(String, SeedOutcome)>, DemoSeedError> {
    let mut results = Vec::new();
    let dirs = list_demo_dirs(root)?;
    if dirs.is_empty() {
        tracing::info!(root = %root.display(), "no demos found");
        return Ok(results);
    }

    // Workspace prerequisites, before any demo compiles: the seeder must be a
    // member of the demo workspace (so the resource resolver resolves), and
    // resource fixtures a demo binds must already exist. Both best-effort —
    // a failure here only blocks the resource-dependent demos, never startup.
    if let Err(e) = ensure_seeder_workspace_membership(state).await {
        tracing::warn!(error = %e, "demo seeder: ensure workspace membership failed");
    }
    seed_demo_resources(state, root).await;
    // After resources: enroll the seeded human-capacity roster (docs/33). A
    // `human`-preset capacity (e.g. `reviewers`) only OFFERS work to members
    // enrolled in it, so a HumanTask-on-offer demo needs its dev member on the
    // roster before that task can ever surface in an inbox. Each fixture's
    // capacity is resolved by path against the rows `seed_demo_resources` just
    // wrote, so this MUST run after it.
    seed_demo_roster(state, root).await;
    // After resources: the model_states projection's `registry_resource_id`
    // resolves a `model_registry` resource alias to its row id (model-pool P1).
    seed_model_states(state, root).await;
    seed_demo_capability_types(state, root).await;
    seed_demo_assets(state, root).await;
    // A SubWorkflow demo can reference a CHILD demo whose directory sorts
    // AFTER it (e.g. `09-agent-tool-loop` -> `09b-collect-feedback`: `-` <
    // `b`, so the parent is attempted first and its child isn't published
    // yet -> `subworkflow_unresolved`). A single in-order pass would skip
    // such a parent forever. Instead, re-attempt the demos that failed as
    // long as each pass resolves at least one more — child/parent directory
    // ordering then stops mattering, no topological sort needed. The loop
    // terminates when nothing is pending or a full pass makes no progress
    // (the remaining failures are genuine, e.g. a missing workspace
    // resource), at which point those are logged best-effort.
    let mut pending: Vec<std::path::PathBuf> = dirs;
    loop {
        let mut still_failed: Vec<std::path::PathBuf> = Vec::new();
        let mut last_errs: Vec<(String, String)> = Vec::new();
        let mut progressed = false;
        for dir in pending {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string());
            match seed_one(state, &dir).await {
                Ok(outcome) => {
                    match outcome {
                        SeedOutcome::AlreadyPresent => tracing::info!(
                            demo = %name,
                            "demo already present — leaving as-is"
                        ),
                        SeedOutcome::Seeded => tracing::info!(
                            demo = %name,
                            "demo seeded"
                        ),
                    }
                    results.push((name, outcome));
                    progressed = true;
                }
                Err(e) => {
                    // Hold the failure for a possible retry next pass — a
                    // child demo published later in THIS pass may unblock it.
                    last_errs.push((name, e.to_string()));
                    still_failed.push(dir);
                }
            }
        }
        if still_failed.is_empty() {
            break;
        }
        if !progressed {
            // No demo resolved this pass — the remaining failures won't be
            // helped by another retry. Log them best-effort and stop: a
            // demo not seeding must not prevent the service from serving.
            for (name, err) in last_errs {
                tracing::warn!(demo = %name, error = %err, "demo seed failed");
            }
            break;
        }
        pending = still_failed;
    }
    Ok(results)
}

/// Seed one demo directory. Idempotent — see [`seed_all`] for the
/// existence check semantics.
pub async fn seed_one(state: &crate::AppState, dir: &Path) -> Result<SeedOutcome, DemoSeedError> {
    let demo = load_demo(dir)?;
    let template_id: uuid::Uuid = demo
        .metadata
        .template_id
        .parse()
        .map_err(|_| DemoSeedError::InvalidTemplateId(demo.metadata.template_id.clone()))?;

    // Resolve + validate the declared visibility against the same invariant
    // the DB CHECK enforces (`private` ⇔ owner present). Absent ⇒ `public`,
    // the historical seed default. Doing it here turns a malformed demo.json
    // into an actionable seed-error line instead of an opaque constraint
    // violation on INSERT.
    let visibility = demo.metadata.visibility.as_deref().unwrap_or("public");
    let owner_template_id: Option<uuid::Uuid> = match (visibility, &demo.metadata.owner_template_id)
    {
        ("workspace" | "public", None) => None,
        ("workspace" | "public", Some(_)) => return Err(DemoSeedError::OwnerOnNonPrivate),
        ("private", None) => return Err(DemoSeedError::PrivateMissingOwner),
        ("private", Some(owner)) => Some(
            owner
                .parse()
                .map_err(|_| DemoSeedError::InvalidOwnerTemplateId(owner.clone()))?,
        ),
        (other, _) => return Err(DemoSeedError::InvalidVisibility(other.to_string())),
    };

    // Idempotency: the stable id is the contract with the rest of the
    // platform (frontend lookup, e2e tests, hand-edited copies). If a
    // row already exists under it — whether seeded last boot or
    // hand-edited since — the seeder must not clobber it.
    let exists: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM workflow_templates WHERE id = $1")
            .bind(template_id)
            .fetch_optional(&state.db)
            .await?;
    if exists.is_some() {
        return Ok(SeedOutcome::AlreadyPresent);
    }

    // From here on, this mirrors `apply_template`'s seed-mode path:
    // compile → upload → INSERT born-published row → init Y.Doc →
    // register triggers live. Each step is logged on failure but no
    // partial state is persisted before commit (S3 orphans are inert).
    let mut files = demo.files.clone();
    let publisher = crate::process::publish::PublishService::new(state);
    let crate::process::publish::CompiledArtifacts {
        air_json,
        graph_json,
        interface_json,
        node_configs,
    } = publisher
        .compile_artifacts(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            template_id,
            1,
            Some(template_id),
            &mut files,
            DEMO_SEEDER_AUTHOR_ID,
            DEMO_WORKSPACE_ID,
        )
        .await
        .map_err(|e| DemoSeedError::Compile(format!("{e:?}")))?;

    publisher
        .upload_files(template_id, 1, &files)
        .await
        .map_err(DemoSeedError::Upload)?;
    publisher
        .upload_node_configs(template_id, 1, &node_configs)
        .await
        .map_err(DemoSeedError::Upload)?;

    // INSERT born-published, version 1, latest. Schema matches the row
    // `apply_template`'s seed-mode finalize produces, just done as a
    // single INSERT since no draft predecessor exists.
    let row: crate::models::template::WorkflowTemplate = sqlx::query_as(
        r#"
        INSERT INTO workflow_templates
            (id, name, description, base_template_id, version,
             is_latest, published, published_at, graph, air_json,
             interface_json, author_id, workspace_id, visibility, owner_template_id)
        VALUES ($1, $2, $3, $1, 1, TRUE, TRUE, NOW(), $4, $5, $6, $7, $8, $9, $10)
        RETURNING *
        "#,
    )
    .bind(template_id)
    .bind(&demo.metadata.name)
    .bind(demo.metadata.description.as_deref().unwrap_or(""))
    .bind(&graph_json)
    .bind(&air_json)
    .bind(&interface_json)
    .bind(DEMO_SEEDER_AUTHOR_ID)
    .bind(DEMO_WORKSPACE_ID)
    .bind(visibility)
    .bind(owner_template_id)
    .fetch_one(&state.db)
    .await?;

    // File the demo into its declared folder (default "Demos"). Keyed on
    // `base_template_id` (= `template_id` here, since this is v1), so the home
    // follows the live `is_latest` version automatically. Best-effort: a
    // filing failure must not fail the seed.
    //
    // Private children are skipped: they're hidden from the catalogue, so a
    // `template_folders` row for them would be dead weight, not a demo a user
    // can open from the folder view.
    if visibility != "private" {
        match ensure_demo_folder(state, demo.metadata.folder.as_deref()).await {
            Ok(folder_id) => {
                if let Err(e) = sqlx::query(
                    "INSERT INTO template_folders (base_template_id, folder_id, workspace_id, moved_by) \
                          VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (base_template_id) \
                          DO UPDATE SET folder_id = EXCLUDED.folder_id, \
                                        workspace_id = EXCLUDED.workspace_id, \
                                        moved_by = EXCLUDED.moved_by, \
                                        moved_at = NOW()",
                )
                .bind(template_id)
                .bind(folder_id)
                .bind(DEMO_WORKSPACE_ID)
                .bind(DEMO_SEEDER_AUTHOR_ID)
                .execute(&state.db)
                .await
                {
                    tracing::warn!(template_id = %template_id, error = %e, "file demo into folder failed (skipped)");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "ensure demo folder failed — demo seeded without folder grouping");
            }
        }
    }

    // Initialize Y.Doc so the web editor sees the same graph + files the
    // executor will run. Non-fatal on failure (the executor reads AIR
    // from S3, not the Y.Doc) but a missing Y.Doc means the editor opens
    // an empty workspace.
    if let Err(e) = state
        .yjs
        .persistence
        .init_doc_from_graph_with_files(template_id, &demo.graph, &files)
        .await
    {
        tracing::warn!(template_id = %template_id, error = %e, "y.doc init failed for seeded demo");
    }

    // Make the demo's triggers live in the in-memory dispatcher
    // immediately. Otherwise `hydrate()`-only behavior would skip them
    // until the next service restart.
    let n = publisher.register_triggers(&row).await;
    if n > 0 {
        tracing::info!(template_id = %template_id, triggers = n, "demo triggers registered live");
    }

    // Seed any bundled template tests. Attached to the family root
    // (`template_id` here since this is v1, so `base_template_id == id`).
    // Failures are per-test best-effort — a malformed test fixture must
    // not block the rest of the demo from being usable.
    for test in &demo.tests {
        let res = sqlx::query(
            r#"
            INSERT INTO template_tests
                (template_id, name, enabled, start_tokens, human_answers, assertions, created_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(template_id)
        .bind(&test.name)
        .bind(test.enabled)
        .bind(&test.start_tokens)
        .bind(&test.human_answers)
        .bind(&test.assertions)
        .bind(DEMO_SEEDER_AUTHOR_ID)
        .execute(&state.db)
        .await;
        match res {
            Ok(_) => tracing::info!(
                template_id = %template_id,
                test_name = %test.name,
                "demo test seeded"
            ),
            Err(e) => tracing::warn!(
                template_id = %template_id,
                test_name = %test.name,
                error = %e,
                "demo test seed failed (skipped)"
            ),
        }
    }

    Ok(SeedOutcome::Seeded)
}

// ── Reset / reseed ────────────────────────────────────────────────────────────

/// Tally of a [`purge_seeded`] / [`reseed_all`] run. Serializable so the admin
/// HTTP endpoint hands it straight back to the operator / CLI.
#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DemoResetReport {
    /// Distinct seeded template *families* (by base id) deleted.
    pub families_removed: usize,
    /// Workflow instances (across every version of those families) whose
    /// engine nets were purged and DB rows deleted.
    pub instances_purged: usize,
    /// Bundled template-test rows removed alongside the families.
    pub tests_removed: usize,
    /// Demos freshly re-seeded. Only ever non-zero from [`reseed_all`].
    pub seeded: usize,
}

/// Force-delete every seeded demo family — the destructive half of "reset
/// demos to pristine". Mirrors `handlers::templates::delete_template`'s cascade
/// but **without** its running-instance guard: this is a deliberate reset, so
/// running instances are cancelled (engine net purged), not spared.
///
/// "Seeded" is `author_id = DEMO_SEEDER_AUTHOR_ID` — the synthetic actor every
/// seeded v1 row carries (see [`DEMO_SEEDER_AUTHOR_ID`]). Each such row is
/// resolved to its family *base* id and the family is deleted whole: instances
/// (nets purged + rows dropped), every version in the chain (Y.Doc cascades via
/// FK), in-memory triggers forgotten, and bundled template-tests removed
/// (template_tests has no FK to cascade, so it is deleted explicitly).
///
/// Idempotent: a second call finds nothing and returns a zeroed report.
pub async fn purge_seeded(state: &crate::AppState) -> Result<DemoResetReport, DemoSeedError> {
    let mut report = DemoResetReport::default();

    // Family base ids. Seeded v1 rows set `base_template_id = id`, so COALESCE
    // collapses both NULL-base and self-base rows to the family root.
    let bases: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT DISTINCT COALESCE(base_template_id, id) \
           FROM workflow_templates WHERE author_id = $1",
    )
    .bind(DEMO_SEEDER_AUTHOR_ID)
    .fetch_all(&state.db)
    .await?;

    let purge_events = state.config.cleanup.purge_events;

    for (base,) in bases {
        // Purge engine nets for every instance in the family, then drop the
        // rows. Force: no running-instance guard.
        let instances: Vec<(uuid::Uuid, String)> = sqlx::query_as(
            "SELECT id, net_id FROM workflow_instances \
              WHERE template_id IN \
              (SELECT id FROM workflow_templates WHERE base_template_id = $1 OR id = $1)",
        )
        .bind(base)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        for (_iid, net_id) in &instances {
            crate::lifecycle::cleanup_net(net_id, &state.nats, &state.petri, purge_events).await;
        }

        let del_inst = sqlx::query(
            "DELETE FROM workflow_instances \
              WHERE template_id IN \
              (SELECT id FROM workflow_templates WHERE base_template_id = $1 OR id = $1)",
        )
        .bind(base)
        .execute(&state.db)
        .await?;
        report.instances_purged += del_inst.rows_affected() as usize;

        // Bundled tests key on the family root id (= base); no FK cascade.
        let del_tests = sqlx::query("DELETE FROM template_tests WHERE template_id = $1")
            .bind(base)
            .execute(&state.db)
            .await?;
        report.tests_removed += del_tests.rows_affected() as usize;

        // Folder filing keys on base_template_id with no FK to templates, so
        // unfile explicitly (leaves the now-empty folder in place for the next
        // reseed to reuse, whichever folder it was filed under).
        sqlx::query("DELETE FROM template_folders WHERE base_template_id = $1")
            .bind(base)
            .execute(&state.db)
            .await?;

        // Capture version ids so their triggers can be forgotten post-delete
        // (otherwise a deleted demo's triggers keep firing until restart).
        let version_ids: Vec<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT id FROM workflow_templates WHERE base_template_id = $1 OR id = $1",
        )
        .bind(base)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        sqlx::query("DELETE FROM workflow_templates WHERE base_template_id = $1 OR id = $1")
            .bind(base)
            .execute(&state.db)
            .await?;

        for (vid,) in version_ids {
            state.triggers.forget_template(vid);
        }
        report.families_removed += 1;
    }

    tracing::info!(
        families = report.families_removed,
        instances = report.instances_purged,
        tests = report.tests_removed,
        "purged seeded demos"
    );
    Ok(report)
}

/// Reset seeded demos to pristine: [`purge_seeded`] then [`seed_all`]. Because
/// the purge runs first, the subsequent seed always recreates from disk (no
/// idempotent skip), discarding any user edits — true "reset to factory".
pub async fn reseed_all(
    state: &crate::AppState,
    root: &Path,
) -> Result<DemoResetReport, DemoSeedError> {
    let mut report = purge_seeded(state).await?;
    let outcomes = seed_all(state, root).await?;
    report.seeded = outcomes
        .iter()
        .filter(|(_, o)| matches!(o, SeedOutcome::Seeded))
        .count();
    tracing::info!(seeded = report.seeded, "reseeded demos");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        // CARGO_MANIFEST_DIR is `service/`; the demos directory lives at
        // the repo root.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// The bundled invoice-processing demo must parse end-to-end through
    /// the same types `/api/v1/templates` accepts. Regressions in
    /// `WorkflowNodeData` serde shape (a new required field, a renamed
    /// variant) will fail this with a precise field-name error rather
    /// than a wall of `serde_json` noise at runtime.
    #[test]
    fn invoice_processing_demo_loads() {
        let dir = repo_root().join("demos/invoice-processing");
        let demo = load_demo(&dir).expect("invoice-processing demo must load");
        assert_eq!(demo.metadata.name, "Invoice Processing Demo");
        // Stable id — tests reach for the demo by this id, so changing
        // it should be a deliberate, type-checked break.
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000001"
        );

        // Sanity: at least one Python node has its `main.py` text loaded.
        let extract = demo
            .files
            .get("extract")
            .expect("extract node must have files");
        assert!(
            extract
                .get("main.py")
                .is_some_and(|s| s.contains("set_output")),
            "extract/main.py must be loaded with the SDK calls intact"
        );

        // The trigger id was rewritten from `trigger-placeholder` to a
        // stable id at dump time — assert the placeholder is gone so a
        // regression in the dumper can't ship the unstable form.
        let has_placeholder = demo
            .graph
            .nodes
            .iter()
            .any(|n| n.id == "trigger-placeholder");
        assert!(
            !has_placeholder,
            "trigger-placeholder must be replaced with a stable id at dump time"
        );

        // Sidecar overlay: graph.json carries `steps: []` for HumanTask
        // nodes; `nodes/<id>/task.json` must be merged in. Without this
        // the review node would have zero blocks and the engine would
        // reject the HumanTaskRequest at runtime.
        use crate::models::template::WorkflowNodeData;
        let review = demo
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "review")
            .expect("review node");
        match &review.data {
            WorkflowNodeData::HumanTask { steps, .. } => {
                assert!(
                    !steps.is_empty(),
                    "review.data.steps must be filled from nodes/review/task.json"
                );
            }
            other => panic!("review must be a HumanTask, got {other:?}"),
        }
        // And the sidecar must NOT also ship as a Y.Doc text file under
        // `files["review"]`, otherwise the editor opens it as a tab and
        // it round-trips back to S3 as a step source.
        if let Some(review_files) = demo.files.get("review") {
            assert!(
                !review_files.contains_key("task.json"),
                "task.json must be consumed as a sidecar, not shipped as a node file"
            );
        }
    }

    /// The bundled invoice-processing demo carries a Feature-B Repeater
    /// block on `manager-approval` that iterates `extract.line_items[*]`.
    /// This pins the demo's full compile path — load + sidecar merge +
    /// validate (including Repeater's typed-error gates) + lower — so a
    /// regression in any of those layers fails here instead of at
    /// `MEKHAN__DEMOS__SEED=true` startup time on a developer's machine.
    #[test]
    fn invoice_processing_demo_compiles_with_repeater() {
        use crate::compiler::{compile_to_air, node_files_inline};
        use crate::models::template::{TaskBlockConfig, WorkflowNodeData};

        let demo = load_demo(&repo_root().join("demos/invoice-processing"))
            .expect("invoice-processing demo must load");

        // Sidecar overlay: `nodes/manager-approval/task.json` must have
        // merged a Repeater block into manager-approval's steps. Without
        // the overlay we'd skip the most interesting compile path below.
        let manager = demo
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "manager-approval")
            .expect("manager-approval node");
        match &manager.data {
            WorkflowNodeData::HumanTask { steps, .. } => {
                let has_repeater = steps
                    .iter()
                    .flat_map(|s| s.blocks.iter())
                    .any(|b| matches!(b, TaskBlockConfig::Repeater { .. }));
                assert!(
                    has_repeater,
                    "manager-approval must carry a Repeater block from \
                     nodes/manager-approval/task.json — found steps: {steps:?}",
                );
            }
            other => panic!("manager-approval must be a HumanTask, got {other:?}"),
        }

        // Full compile through the same path mekhan-service uses at
        // publish time — surfaces RepeaterRef* errors as a hard fail
        // here rather than a runtime engine wedge.
        let files = node_files_inline(&demo.files);
        let _air = compile_to_air(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
        )
        .expect("invoice-processing must compile (Repeater + sidecar overlays)");
    }

    /// The bundled document-pipeline-v1 demo's graph.json must parse and the
    /// merge-extraction node must deserialize as a `Join { mode: Any }`
    /// (XOR-join — the primitive introduced alongside this test). Then
    /// re-serialize and round-trip through the standard `WorkflowGraph` /
    /// `WorkflowNodeData` serde shapes. Sidecar-overlay loading is exercised
    /// separately by invoice_processing_demo_loads — this test isolates the
    /// Join variant so a regression in its serde discriminant or field
    /// defaults surfaces here, not buried under unrelated sidecar parse noise.
    #[test]
    fn document_pipeline_v1_join_node_round_trips() {
        use crate::models::template::{JoinMode, WorkflowGraph, WorkflowNodeData};

        let graph_path = repo_root().join("demos/document-pipeline-v1/graph.json");
        let raw = std::fs::read_to_string(&graph_path).expect("graph.json must exist");
        let graph: WorkflowGraph =
            serde_json::from_str(&raw).expect("graph.json must deserialize as WorkflowGraph");

        let merge = graph
            .nodes
            .iter()
            .find(|n| n.id == "merge-extraction")
            .expect("merge-extraction node must exist");

        match &merge.data {
            WorkflowNodeData::Join { mode, output, .. } => {
                assert_eq!(*mode, JoinMode::Any, "demo uses XOR-join (mode=any)");
                assert!(
                    output.fields.iter().any(|f| f.name == "fields"),
                    "merge-extraction.output must declare a `fields` field — \
                     persist/main.py borrows extraction.fields through it"
                );
            }
            other => panic!("merge-extraction must be a Join, got {other:?}"),
        }
    }

    /// Regression: the document-pipeline-v1 demo carries five LLM extractor
    /// nodes whose `response_format.schema` resolves to a $ref against a
    /// deeply-nested `ExtractionFields` definition. Before the
    /// `config_ref` offload, each extractor's prepare-transition Rhai
    /// literal would blow Rhai's expression-complexity limit and the demo
    /// failed to seed. This test exercises the full compile path against
    /// the actual demo file and asserts that every LLM node parks its
    /// config in the side-channel — proving the panic is gone.
    #[test]
    fn document_pipeline_v1_compiles_with_strict_schemas() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };

        let demo = load_demo(&repo_root().join("demos/document-pipeline-v1"))
            .expect("document-pipeline-v1 must load");

        let files = node_files_inline(&demo.files);
        let CompileArtifacts { node_configs, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("document-pipeline-v1 must compile (no Rhai-complexity panic)");

        // Every LLM extractor's resolved config must land in the
        // side-channel — that's the proof the literal-inline path is
        // gone. The classify node uses response_format: text (no schema),
        // but the five extractors each carry a deeply-nested
        // ExtractionFields shape.
        for node_id in [
            "classify",
            "extract-bloodwork",
            "extract-prescription",
            "extract-clinical-note",
            "extract-form-fields",
            "extract-generic",
        ] {
            assert!(
                node_configs.contains_key(node_id),
                "node config for `{node_id}` must be parked in side-channel; got keys: {:?}",
                node_configs.keys().collect::<Vec<_>>()
            );
        }
        // And the heavy `$ref`-expanded schema must have made it into the
        // side-channel (proves `inline_refs` ran before the offload).
        let bw = node_configs
            .get("extract-bloodwork")
            .expect("extract-bloodwork config")
            .to_string();
        assert!(
            bw.contains("ocr_span"),
            "extract-bloodwork config must contain the expanded ExtractionFields schema: {bw}"
        );
        assert!(
            !bw.contains("\"$ref\""),
            "$ref must have been inlined before parking: {bw}"
        );
    }

    /// `document-pipeline-branching-v1` is the Phase-1 branching pipeline:
    /// vision-LLM classify (on the raw file, BEFORE OCR) → `decision`
    /// route-by-class → two branches (bloodwork: Surya OCR → OCR-text extract
    /// → Python resolve-bbox; generic: single vision extract) → XOR-`join`
    /// (mode=any) → Python validate → end. It exercises four things together
    /// on a real bundled fixture: (1) a Surya `automated_step` (the
    /// `{{ start.document_file }}` placeholder bypasses the node-file gate),
    /// (2) the strict `$ref` ExtractionFields schema parked in the
    /// side-channel, (3) two Python nodes whose `main.py` reads NON-predecessor
    /// producers via bare `slug.field` accesses (`ocr.words`,
    /// `extract_bloodwork.fields`, `extraction.fields`, `classify.document_type`)
    /// — the read-arc synthesis the resolve-bbox cascade depends on, and (4)
    /// the join converging the two branch tails. A break in any of those
    /// layers fails here rather than silently at `MEKHAN__DEMOS__SEED=true`
    /// startup. Mirrors `document_pipeline_v1_compiles_with_strict_schemas`.
    #[test]
    fn document_pipeline_branching_v1_compiles_with_strict_schemas() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };
        use crate::models::template::{JoinMode, WorkflowNodeData};

        let demo = load_demo(&repo_root().join("demos/document-pipeline-branching-v1"))
            .expect("document-pipeline-branching-v1 must load");
        assert_eq!(demo.metadata.name, "Document Pipeline — Branching v1");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000054"
        );

        // Stable trigger node id — tests + the dispatcher registry key off it,
        // so drift must be a deliberate, type-checked break.
        assert!(
            demo.graph
                .nodes
                .iter()
                .any(|n| n.id == "trg_document_pipeline_branching_v1"),
            "trigger node id must be trg_document_pipeline_branching_v1"
        );

        // The XOR-join the two branch tails fan into.
        let merge = demo
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "merge-extraction")
            .expect("merge-extraction node must exist");
        match &merge.data {
            WorkflowNodeData::Join { mode, output, .. } => {
                assert_eq!(
                    *mode,
                    JoinMode::Any,
                    "branching demo uses XOR-join (mode=any)"
                );
                assert!(
                    output.fields.iter().any(|f| f.name == "fields"),
                    "merge-extraction.output must declare a `fields` field"
                );
            }
            other => panic!("merge-extraction must be a Join, got {other:?}"),
        }

        // Both Python nodes must ship their main.py with the SDK calls intact.
        for node_id in ["resolve-bbox", "validate"] {
            let node_files = demo
                .files
                .get(node_id)
                .unwrap_or_else(|| panic!("{node_id} node must have files"));
            assert!(
                node_files
                    .get("main.py")
                    .is_some_and(|s| s.contains("set_output")),
                "{node_id}/main.py must be loaded with the SDK calls intact"
            );
        }

        let files = node_files_inline(&demo.files);
        let CompileArtifacts { node_configs, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("document-pipeline-branching-v1 must compile (no Rhai-complexity panic)");

        // Both LLM extractors park their resolved config (the strict
        // ExtractionFields `$ref` would blow Rhai's complexity limit inline).
        for node_id in ["classify", "extract-bloodwork", "extract-generic"] {
            assert!(
                node_configs.contains_key(node_id),
                "node config for `{node_id}` must be parked in side-channel; got keys: {:?}",
                node_configs.keys().collect::<Vec<_>>()
            );
        }
        // The heavy `$ref`-expanded schema must have made it into the
        // side-channel with the `$ref` inlined first.
        let bw = node_configs
            .get("extract-bloodwork")
            .expect("extract-bloodwork config")
            .to_string();
        assert!(
            bw.contains("reference_range"),
            "extract-bloodwork config must contain the expanded ExtractionFields schema: {bw}"
        );
        assert!(
            !bw.contains("\"$ref\""),
            "$ref must have been inlined before parking: {bw}"
        );
    }

    /// Compile the `document-pipeline-branching-v1` demo to AIR and dump the
    /// canonical JSON to a file so the clinic can ship the **compiler output**
    /// (not a hand-authored net). Gated on `DUMP_BRANCHING_AIR=<path>`: when
    /// the env var is set, the test writes the AIR to that path; otherwise it
    /// is a no-op (keeps CI quiet). This is the single export step in the
    /// graph.json → clinic-AIR regeneration flow — see the demo README.
    ///
    /// The clinic ships this AIR through `apply-workflows.sh` →
    /// `POST /api/v1/templates/apply-air`, which stores the AIR **verbatim**
    /// and uploads **nothing** to S3 (the endpoint is opaque to
    /// mekhan-service). The compiler's default lowering parks every node's
    /// static config in the S3 side-channel and emits a `config_ref`
    /// `storage_path` — which would dangle in the clinic path because no
    /// publish-time upload runs. So this dumper post-processes the AIR to
    /// **inline** each parked config back into its prepare-transition Rhai
    /// (`config_ref { storage_path }` → `config { … }`), using the
    /// `node_configs` side-channel the compiler returns. Per
    /// `executor-domain::ExecutionSpec`, an inline `config` with no
    /// `config_ref` is a first-class, fully-supported path (the executor's
    /// `FetchConfigHook` is skipped). The Python `main.py` source is already
    /// inlined by the lowering (`inputs[].source.content`), so the resulting
    /// AIR is fully self-contained — no S3 dependency.
    #[test]
    fn dump_document_pipeline_branching_v1_air() {
        use crate::compiler::rhai_gen::json_to_rhai_literal;
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
            ConfigStorage,
        };

        let Some(out_path) = std::env::var_os("DUMP_BRANCHING_AIR") else {
            return;
        };

        let demo = load_demo(&repo_root().join("demos/document-pipeline-branching-v1"))
            .expect("document-pipeline-branching-v1 must load");

        let files = node_files_inline(&demo.files);
        let CompileArtifacts {
            mut air,
            node_configs,
            ..
        } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("document-pipeline-branching-v1 must compile to AIR");

        // Inline every parked config back into its prepare-transition Rhai so
        // the AIR carries no `config_ref` storage dependency. The compiler
        // minted each `config_ref` as the literal
        //   "config_ref": #{ "storage_path": "<key>" }
        // inside the `<node>/prepare` transition's logic source. We replace
        // that exact substring with
        //   "config": <rhai literal of the parked config>
        // and assert each replacement landed (so a lowering change to the
        // emitted shape fails loudly here rather than shipping a dangling ref).
        let transitions = air
            .get_mut("transitions")
            .and_then(|v| v.as_array_mut())
            .expect("AIR must carry a transitions array");
        let mut inlined = 0usize;
        for (node_id, config) in &node_configs {
            let storage_key = ConfigStorage::ephemeral().key(node_id);
            let needle = format!(
                "\"config_ref\": #{{ \"storage_path\": \"{}\" }}",
                storage_key.replace('\\', "\\\\").replace('"', "\\\"")
            );
            let replacement = format!("\"config\": {}", json_to_rhai_literal(config));

            let prepare_id = format!("{node_id}/prepare");
            let t = transitions
                .iter_mut()
                .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(prepare_id.as_str()))
                .unwrap_or_else(|| panic!("prepare transition `{prepare_id}` must exist"));
            let src = t
                .pointer_mut("/logic/source")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| panic!("`{prepare_id}` must carry a Rhai logic.source"));
            assert!(
                src.contains(&needle),
                "`{prepare_id}` Rhai must contain the config_ref literal to inline; \
                 looked for: {needle}"
            );
            let rewritten = src.replace(&needle, &replacement);
            *t.pointer_mut("/logic/source").unwrap() = serde_json::Value::String(rewritten);
            inlined += 1;
        }
        assert_eq!(
            inlined,
            node_configs.len(),
            "every parked node config must be inlined"
        );

        // No dangling storage refs may remain in any executable transition
        // logic (proves the inlining was total). Note: the `definitions` block
        // carries the *schema* for the ExecutorSubmitInput token, whose shape
        // legitimately includes an (unused, None) `config_ref` field plus
        // doc-comments that name `config_ref` / the `node-config.json` key
        // pattern — that is inert token-type metadata, not a live storage
        // reference. The load-bearing signal that a real parked-config offload
        // survived is a `node-config.json` storage_path inside a
        // `transition.logic.source`, so we assert on that surface only.
        for t in air
            .get("transitions")
            .and_then(|v| v.as_array())
            .expect("transitions array")
        {
            if let Some(src) = t.pointer("/logic/source").and_then(|v| v.as_str()) {
                let tid = t.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                assert!(
                    !src.contains("node-config.json"),
                    "transition `{tid}` logic.source still references a node-config.json \
                     storage path — inlining missed it"
                );
                assert!(
                    !src.contains("config_ref"),
                    "transition `{tid}` logic.source still references config_ref — \
                     inlining missed it"
                );
            }
        }

        let canonical = serde_json::to_string_pretty(&air).expect("AIR must serialize");
        std::fs::write(&out_path, format!("{canonical}\n"))
            .unwrap_or_else(|e| panic!("write AIR to {out_path:?}: {e}"));
        eprintln!(
            "wrote self-contained branching AIR ({} bytes, {} configs inlined) to {:?}",
            canonical.len(),
            inlined,
            out_path
        );
    }

    /// `classify-and-group-v1` is the strict prefix of `document-pipeline-v1`
    /// (OCR → vision-LLM classify; no extract / persist / verify). The demo
    /// carries a strict `GroupedClassification` `$ref` response_format schema
    /// — same code path the v1 demo's extractors use, so we exercise the
    /// `$ref` expansion + side-channel parking in isolation, without the
    /// full DAG noise. A break here means the schema-expansion bug from
    /// `document_pipeline_v1_compiles_with_strict_schemas` regressed for
    /// the single-node case.
    #[test]
    fn classify_and_group_v1_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };

        let demo = load_demo(&repo_root().join("demos/classify-and-group-v1"))
            .expect("classify-and-group-v1 must load");
        assert_eq!(demo.metadata.name, "Classify & Group v1");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000051"
        );

        let files = node_files_inline(&demo.files);
        let CompileArtifacts { node_configs, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("classify-and-group-v1 must compile");

        // The vision-LLM `classify` step uses a strict response_format
        // `$ref`. Its resolved config must land in the side-channel
        // (would-be Rhai-complexity blow-up otherwise) and the `$ref`
        // must be inlined before parking.
        let classify_cfg = node_configs
            .get("classify")
            .expect("classify config must be parked")
            .to_string();
        assert!(
            classify_cfg.contains("page_ids"),
            "classify config must contain the expanded GroupedClassification schema: {classify_cfg}"
        );
        assert!(
            !classify_cfg.contains("\"$ref\""),
            "$ref must be inlined before parking: {classify_cfg}"
        );

        // OCR borrow: `{{ ocr.content }}` in the classify prompt becomes
        // `{{input:__borrow_ocr__content}}` after the borrow rewrite.
        // Mirrors the equivalent assertion in
        // `ocr_classify_extract_demo_loads_and_compiles_with_borrows`.
        assert!(
            classify_cfg.contains("__borrow_ocr__content"),
            "classify config must carry the OCR borrow rewrite: {classify_cfg}"
        );
    }

    /// `di-extraction-canary` is the deliberately-degraded "single vision
    /// call → coerce" demo. It pairs an LLM step with a strict
    /// `RawExtraction` response_format `$ref` AND a Python `validate` node
    /// — so it exercises the side-channel parking for $ref schemas alongside
    /// the Python-staging path on the same demo. The Python `validate` step
    /// borrows from the LLM via direct slug access (no template
    /// placeholders), so we don't assert on borrow rewrites here; the
    /// compile pass succeeding is the proof.
    #[test]
    fn di_extraction_canary_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };

        let demo = load_demo(&repo_root().join("demos/di-extraction-canary"))
            .expect("di-extraction-canary must load");
        assert_eq!(demo.metadata.name, "DI Extraction Canary");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000052"
        );

        // The Python validate node must ship with its main.py loaded.
        let validate_files = demo
            .files
            .get("validate")
            .expect("validate node must have files");
        assert!(
            validate_files
                .get("main.py")
                .is_some_and(|s| s.contains("set_output")),
            "validate/main.py must be loaded with the SDK calls intact"
        );

        let files = node_files_inline(&demo.files);
        let CompileArtifacts { node_configs, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("di-extraction-canary must compile");

        // The vision LLM `extract` step has a strict $ref response_format.
        let extract_cfg = node_configs
            .get("extract")
            .expect("extract config must be parked")
            .to_string();
        assert!(
            extract_cfg.contains("document_type"),
            "extract config must contain the expanded RawExtraction schema: {extract_cfg}"
        );
        assert!(
            !extract_cfg.contains("\"$ref\""),
            "$ref must be inlined before parking: {extract_cfg}"
        );
    }

    /// GAP F: `37-internal-pool-agent` authors `provider: "internal"` in the
    /// graph (so the editor round-trips it as an internal-router binding), but
    /// the compiled WIRE config the executor receives must carry `openai` (its
    /// `Provider` enum rejects `internal`). The degenerate agent path
    /// synthesizes an `AutomatedStep(Llm)` and the LLM validator remaps the
    /// provider; this pins that the parked config emits `openai` while keeping
    /// the `internal_pool_router` resource_alias that overlays the router.
    #[test]
    fn internal_pool_agent_demo_compiles_to_openai_wire() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };

        let demo = load_demo(&repo_root().join("demos/37-internal-pool-agent"))
            .expect("37-internal-pool-agent must load");

        let files = node_files_inline(&demo.files);
        let CompileArtifacts { node_configs, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("37-internal-pool-agent must compile");

        let agent_cfg = node_configs
            .get("agent")
            .expect("agent config must be parked");
        assert_eq!(
            agent_cfg.get("provider").and_then(|v| v.as_str()),
            Some("openai"),
            "internal provider must compile to the openai wire shape: {agent_cfg}"
        );
        assert_eq!(
            agent_cfg.get("resource_alias").and_then(|v| v.as_str()),
            Some("internal_pool_router"),
            "the router-binding alias must survive the remap: {agent_cfg}"
        );
    }

    /// `loki-error-alert` is the scheduled-alert composition: a Cron trigger
    /// fires `Start{fire_time}` → a Loki `query_range` AutomatedStep →
    /// a single-shot Agent that summarizes the matched entries →
    /// an SMTP AutomatedStep that emails the summary. This pins that the
    /// full chain compiles and that the SMTP step carries the agent-output
    /// borrow (`{{ summarize.response }}`) into its parked config.
    #[test]
    fn loki_error_alert_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };

        let demo = load_demo(&repo_root().join("demos/loki-error-alert"))
            .expect("loki-error-alert must load");
        assert_eq!(demo.metadata.name, "Loki Error-Log Alert");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-0000000000a1"
        );

        let files = node_files_inline(&demo.files);
        let CompileArtifacts { node_configs, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("loki-error-alert must compile");

        // The Loki step binds the cluster resource.
        let loki_cfg = node_configs
            .get("query_logs")
            .expect("query_logs config must be parked")
            .to_string();
        assert!(
            loki_cfg.contains("aithericon_loki"),
            "loki step must bind the aithericon_loki resource: {loki_cfg}"
        );

        // The SMTP step keeps the literal Tera placeholders in its parked
        // config (resolved at render time from the staged producer envelopes
        // via synthesized read-arcs) — so the email body must still reference
        // both the agent summary and the upstream entry count.
        let smtp_cfg = node_configs
            .get("send_alert")
            .expect("send_alert config must be parked")
            .to_string();
        assert!(
            smtp_cfg.contains("{{ summarize.response }}"),
            "smtp step must template against the agent summary: {smtp_cfg}"
        );
        assert!(
            smtp_cfg.contains("{{ query_logs.entry_count }}"),
            "smtp step must template against the upstream entry count: {smtp_cfg}"
        );

        // The agent binds the in-cluster inference router: `provider: internal`
        // must remap to the `openai` wire shape while keeping the
        // `internal_pool_router` resource_alias that overlays the router
        // endpoint at fire time (same contract as demo 37).
        let agent_cfg = node_configs
            .get("summarize")
            .expect("summarize config must be parked");
        assert_eq!(
            agent_cfg.get("provider").and_then(|v| v.as_str()),
            Some("openai"),
            "internal provider must compile to the openai wire shape: {agent_cfg}"
        );
        assert_eq!(
            agent_cfg.get("resource_alias").and_then(|v| v.as_str()),
            Some("internal_pool_router"),
            "the router-binding alias must survive the remap: {agent_cfg}"
        );
    }

    /// `output-safety-gate` is a SubWorkflow-shaped composable critic:
    /// a low-temperature LLM critic step (with a strict `CriticFlags` `$ref`
    /// response_format) followed by two Python steps (`verify`, `decide`)
    /// that share the same `nodes/<id>/main.py` layout the other Python
    /// demos use. This test pins down the three-node compile path on the
    /// shipped fixture — the same code the parent SubWorkflow caller would
    /// hit at publish time.
    #[test]
    fn output_safety_gate_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };

        let demo = load_demo(&repo_root().join("demos/output-safety-gate"))
            .expect("output-safety-gate must load");
        assert_eq!(demo.metadata.name, "Output Safety Gate");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000053"
        );

        // Both Python nodes must ship their main.py.
        for node_id in ["verify", "decide"] {
            let node_files = demo
                .files
                .get(node_id)
                .unwrap_or_else(|| panic!("{node_id} node must have files"));
            assert!(
                node_files
                    .get("main.py")
                    .is_some_and(|s| s.contains("set_output")),
                "{node_id}/main.py must be loaded with the SDK calls intact"
            );
        }

        let files = node_files_inline(&demo.files);
        let CompileArtifacts { node_configs, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("output-safety-gate must compile");

        // Critic config must have the expanded CriticFlags schema parked
        // and the start.subject_text / start.evidence_text borrows wired.
        let critic_cfg = node_configs
            .get("critic")
            .expect("critic config must be parked")
            .to_string();
        assert!(
            critic_cfg.contains("supporting_text"),
            "critic config must contain the expanded CriticFlags schema: {critic_cfg}"
        );
        assert!(
            !critic_cfg.contains("\"$ref\""),
            "$ref must be inlined before parking: {critic_cfg}"
        );
        assert!(
            critic_cfg.contains("__borrow_start__subject_text"),
            "critic config must carry the subject_text borrow rewrite: {critic_cfg}"
        );
        assert!(
            critic_cfg.contains("__borrow_start__evidence_text"),
            "critic config must carry the evidence_text borrow rewrite: {critic_cfg}"
        );
    }

    /// A task sidecar targeting a non-HumanTask node is a typo — fail at
    /// load time with a clear pointer, not at publish time with "engine
    /// rejected empty steps" three calls deep.
    #[test]
    fn task_sidecar_on_non_human_task_node_is_a_hard_error() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();
        // Minimal demo directory: graph with one Start (NOT a HumanTask)
        // + a `task.json` sidecar that *names* `start`. Should reject.
        std::fs::write(
            tmp.path().join("demo.json"),
            r#"{ "templateId": "deadbeef-0000-0000-0000-000000000000", "name": "X" }"#,
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("graph.json"),
            r#"{
                "nodes": [{
                    "id": "start",
                    "type": "start",
                    "position": { "x": 0, "y": 0 },
                    "data": { "type": "start", "label": "Start" }
                }],
                "edges": []
            }"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("nodes/start")).unwrap();
        std::fs::write(tmp.path().join("nodes/start/task.json"), "[]").unwrap();

        let err = load_demo(tmp.path()).expect_err("must reject");
        match err {
            DemoLoadError::TaskSidecarTypeMismatch { node_type, .. } => {
                assert_eq!(node_type, "start");
            }
            other => panic!("expected TaskSidecarTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn list_demo_dirs_finds_invoice_processing() {
        let root = repo_root().join("demos");
        let dirs = list_demo_dirs(&root).expect("demos root must list");
        assert!(
            dirs.iter().any(|p| p.ends_with("invoice-processing")),
            "invoice-processing must appear in {dirs:?}"
        );
    }

    /// The llm-smoke demo (text-only LLM step pointed at a local Ollama
    /// daemon) must parse + compile cleanly through the same AIR pipeline
    /// `/api/v1/templates/{id}/publish` uses. The demo has no node files, so
    /// this also covers the "LLM step with zero staged inputs" case which
    /// the existing learning-path tests don't exercise.
    #[test]
    fn llm_smoke_demo_loads_and_compiles() {
        use crate::compiler::{compile_to_air, node_files_inline};

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("llm-smoke")).expect("llm-smoke must load");
        assert_eq!(demo.metadata.name, "LLM Smoke Test");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000020"
        );

        let files = node_files_inline(&demo.files);
        let air = compile_to_air(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
        )
        .unwrap_or_else(|e| panic!("llm-smoke must compile to AIR: {e:?}"));
        assert!(
            air.to_string().contains("\"transitions\""),
            "llm-smoke AIR must declare transitions"
        );
    }

    /// The 09-agent-tool-loop demo (Start → Agent → End, tool = a SubWorkflow
    /// child) must parse + compile cleanly. Pins the agent loop path: with
    /// `maxTurns: 4` + a tool child the compiler takes `lower_agent_loop`
    /// (NOT the degenerate single-shot path), so the resulting AIR must
    /// carry the loop scaffold's signature places + transitions. The tool is
    /// now a SubWorkflow, so the bare `compile_to_air` can't resolve it (like
    /// 06-subworkflow) — feed a stub child AIR via `SubWorkflowAir`. The loop
    /// markers come from the agent + the slugified tool label (`lookup_order`),
    /// independent of the child's contents.
    #[test]
    fn agent_tool_loop_demo_loads_and_compiles() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileOptions, ResolvedChild,
            SubWorkflowAir,
        };

        let root = repo_root().join("demos");
        let demo =
            load_demo(&root.join("09-agent-tool-loop")).expect("09-agent-tool-loop must load");
        assert_eq!(demo.metadata.name, "09 · Agent + Tool Loop");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000019"
        );

        // Stub the SubWorkflow tool child ("lookup_order" node) — only its
        // presence matters for the loop-marker assertions below.
        let mut sub_air = SubWorkflowAir::new();
        sub_air.insert(
            "lookup_order".to_string(),
            ResolvedChild {
                air: serde_json::json!({
                    "name": "child-stub", "places": [], "transitions": [],
                    "groups": [], "mock_adapters": [], "definitions": {}, "requirements": []
                }),
                resolved_version: 1,
                template_id: "00000000-0000-0000-0000-00000000008a".to_string(),
                input_contract: crate::models::template::Port::empty_input(),
                output_contract: crate::models::template::Port::empty_input(),
            },
        );
        // Second tool: the `collect_feedback` HumanTask-form SubWorkflow (09b).
        // Same stub treatment — only its presence drives the dispatch/collect
        // marker assertions; the real child's Start/End contract is exercised by
        // 09b's own compile + the live publish path.
        sub_air.insert(
            "collect_feedback".to_string(),
            ResolvedChild {
                air: serde_json::json!({
                    "name": "child-stub", "places": [], "transitions": [],
                    "groups": [], "mock_adapters": [], "definitions": {}, "requirements": []
                }),
                resolved_version: 1,
                template_id: "00000000-0000-0000-0000-00000000009b".to_string(),
                input_contract: crate::models::template::Port::empty_input(),
                output_contract: crate::models::template::Port::empty_input(),
            },
        );

        let files = node_files_inline(&demo.files);
        let air = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                sub_air: &sub_air,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("09-agent-tool-loop must compile to AIR: {e:?}"))
        .air;

        let air_str = air.to_string();
        // Signature transitions of the loop path — degenerate would emit
        // none of these and instead delegate to AutomatedStep's plain
        // lifecycle, so seeing them proves the compiler took the right
        // branch and minted the tool dispatch/collect plumbing.
        for marker in [
            "t_agent_enter",
            "t_agent_prepare_call",
            "t_agent_route_final",
            "t_agent_route_dispatch_lookup_order",
            "t_agent_invoke_lookup_order",
            "t_agent_collect_lookup_order",
            "p_agent_state",
            "p_agent_dispatch_lookup_order",
            // Second tool — the dynamic-form HumanTask SubWorkflow. Proves the
            // agent compiler mints dispatch/invoke/collect plumbing for it too.
            "t_agent_route_dispatch_collect_feedback",
            "t_agent_invoke_collect_feedback",
            "t_agent_collect_collect_feedback",
            "p_agent_dispatch_collect_feedback",
        ] {
            assert!(
                air_str.contains(marker),
                "agent loop AIR must contain `{marker}` — compiler skipped the loop path?"
            );
        }
    }

    /// The email-welcome demo (Start → HumanTask intake → SMTP send → End)
    /// must parse + compile cleanly through the same AIR pipeline
    /// `/api/v1/templates/{id}/publish` uses. This is the canonical SMTP-backend
    /// demo: it exercises the placeholder borrow scanner against an inline
    /// Tera template (a path Python doesn't cover) and asserts the SMTP
    /// backend dispatches without requiring a real mail server.
    #[test]
    fn email_welcome_demo_loads_and_compiles() {
        use crate::compiler::node_files_inline;
        use crate::compiler::resource_refs::{KnownResource, KnownResources};
        use crate::compiler::{compile_to_air_with_options, CompileArtifacts, CompileOptions};
        use std::collections::HashMap;
        use uuid::Uuid;

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("email-welcome")).expect("email-welcome must load");
        assert_eq!(demo.metadata.name, "Email Welcome");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000030"
        );

        let files = node_files_inline(&demo.files);
        // Mirror the publish-path call: `apply_template` passes
        // `discover_known_resources` output (workspace-resolved map of
        // resource heads → DB rows). For the demo we pre-populate `mail`
        // since the demo declares `resource_alias: "mail"`.
        let mut known = KnownResources::new();
        known.insert(
            "mail".to_string(),
            KnownResource {
                id: Uuid::new_v4(),
                type_name: "smtp".to_string(),
                latest_version: 1,
                public_config: serde_json::Value::Null,
            },
        );
        let inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let known_globals = crate::compiler::named_global::globals_from_resources(&known);
        let CompileArtifacts { air, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &inline,
                known_globals: &known_globals,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| {
            panic!("email-welcome must compile to AIR with known resources: {e:?}")
        });

        let send_prepare = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions")
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("send/prepare"))
            .expect("send/prepare exists");
        let logic_node = send_prepare.get("logic").expect("send/prepare logic");
        let source = logic_node
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| logic_node.get("source").and_then(|s| s.as_str()))
            .expect("Rhai source");

        // Upstream producer borrow (HumanTask → SMTP step).
        assert!(
            source.contains("intake.json"),
            "compiled AIR must stage intake.json for the Tera template context"
        );
        // Resource envelope borrow — the bug this test guards against:
        // upstream-borrow arm consumed BORROW_MARKER before the resource
        // arm could splice, so `mail.json` never landed.
        assert!(
            source.contains("mail.json"),
            "compiled AIR must stage mail.json (resource envelope); source:\n{source}"
        );
        assert!(
            source.contains("__resources[\"mail\"]") || source.contains("__resources['mail']"),
            "compiled AIR must read mail envelope from __resources map; source:\n{source}"
        );
        assert!(
            source.contains("\"smtp\""),
            "compiled AIR must carry the smtp backend discriminator"
        );
    }

    #[test]
    fn http_call_demo_loads_and_compiles_with_borrow() {
        use crate::compiler::node_files_inline;
        use crate::compiler::{compile_to_air_with_options, CompileArtifacts, CompileOptions};
        use std::collections::HashMap;

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("11-http-call")).expect("11-http-call must load");
        assert_eq!(demo.metadata.name, "11 · HTTP Call");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000022"
        );

        let files = node_files_inline(&demo.files);
        // HTTP binds no workspace resource — empty globals, like any
        // resource-free step.
        let inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let CompileArtifacts { air, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &inline,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("11-http-call must compile to AIR: {e:?}"));

        let call_prepare = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions")
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("httpcall/prepare"))
            .expect("httpcall/prepare exists");
        let logic_node = call_prepare.get("logic").expect("httpcall/prepare logic");
        let source = logic_node
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| logic_node.get("source").and_then(|s| s.as_str()))
            .expect("Rhai source");

        // The HTTP step's `{{ intake.username }}` / `{{ intake.topic }}`
        // references (in url/header/query/body) must synthesize a read-arc
        // that stages the intake producer envelope as `intake.json`.
        assert!(
            source.contains("intake.json"),
            "compiled AIR must stage intake.json for the HTTP Tera context; source:\n{source}"
        );
        assert!(
            source.contains("\"http\""),
            "compiled AIR must carry the http backend discriminator"
        );
    }

    /// The Postgres-backend demo (`19-postgres-node`) must parse + compile
    /// through the same AIR pipeline `/api/v1/templates/{id}/publish` uses.
    /// Pins the resource-bound topology: a READ step and a WRITE step both
    /// binding the `demo_pg` postgres resource (ConfigOverlay), each borrowing
    /// a `{{ start.* }}` param. The compiler must stage the Start producer
    /// envelope + the resource envelope and carry the `postgres` discriminator.
    #[test]
    fn postgres_node_demo_loads_and_compiles_with_resource() {
        use crate::compiler::node_files_inline;
        use crate::compiler::resource_refs::{KnownResource, KnownResources};
        use crate::compiler::{compile_to_air_with_options, CompileArtifacts, CompileOptions};
        use std::collections::HashMap;
        use uuid::Uuid;

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("19-postgres-node")).expect("19-postgres-node must load");
        assert_eq!(demo.metadata.name, "19 · Postgres Node");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000170"
        );

        let files = node_files_inline(&demo.files);
        // Both steps declare `resource_alias: "demo_pg"`; pre-populate the
        // workspace-resolved map the publish path would pass.
        let mut known = KnownResources::new();
        known.insert(
            "demo_pg".to_string(),
            KnownResource {
                id: Uuid::new_v4(),
                type_name: "postgres".to_string(),
                latest_version: 1,
                public_config: serde_json::Value::Null,
            },
        );
        let inline: HashMap<String, HashMap<String, String>> = HashMap::new();
        let known_globals = crate::compiler::named_global::globals_from_resources(&known);
        let CompileArtifacts { air, .. } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &inline,
                known_globals: &known_globals,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("19-postgres-node must compile to AIR: {e:?}"));

        let transitions = air
            .get("transitions")
            .and_then(|t| t.as_array())
            .expect("transitions");
        let prepare = transitions
            .iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("read_widgets/prepare"))
            .expect("read_widgets/prepare exists");
        let logic_node = prepare.get("logic").expect("read_widgets/prepare logic");
        let source = logic_node
            .get("Rhai")
            .and_then(|l| l.get("source"))
            .and_then(|s| s.as_str())
            .or_else(|| logic_node.get("source").and_then(|s| s.as_str()))
            .expect("Rhai source");

        // The `{{ start.min_id }}` param borrow stages the Start producer
        // envelope; the bound `demo_pg` resource stages its envelope.
        assert!(
            source.contains("start.json"),
            "compiled AIR must stage start.json for the postgres param borrow; source:\n{source}"
        );
        assert!(
            source.contains("demo_pg.json"),
            "compiled AIR must stage demo_pg.json (resource envelope); source:\n{source}"
        );
        assert!(
            source.contains("\"postgres\""),
            "compiled AIR must carry the postgres backend discriminator"
        );
    }

    /// The Bayesian-optimization loop demo (`12-bo-loop`) must parse through
    /// the same types `/api/v1/templates` accepts. Pins the BO topology: a
    /// Loop carrying four accumulators (observations / f_best / best_a /
    /// best_d, none named `iteration`), a Map (`evals`) nested in the loop
    /// body scattering `propose.candidates`, and the propose/branin/gather
    /// Python nodes.
    #[test]
    fn bo_loop_demo_loads() {
        use crate::models::template::WorkflowNodeData;

        let dir = repo_root().join("demos/12-bo-loop");
        let demo = load_demo(&dir).expect("12-bo-loop demo must load");
        assert_eq!(demo.metadata.name, "12 · Bayesian Optimization Loop");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-0000000000c0"
        );

        // Every load-bearing slug is present on the graph.
        let slug_of = |id: &str| -> Option<String> {
            demo.graph
                .nodes
                .iter()
                .find(|n| n.id == id)
                .and_then(|n| n.slug.clone())
        };
        assert_eq!(
            slug_of("lp").as_deref(),
            Some("bo"),
            "loop slug must be `bo`"
        );
        assert_eq!(slug_of("propose").as_deref(), Some("propose"));
        assert_eq!(
            slug_of("mp").as_deref(),
            Some("evals"),
            "map slug must be `evals`"
        );
        assert_eq!(slug_of("branin").as_deref(), Some("branin"));
        assert_eq!(slug_of("gather").as_deref(), Some("gather"));

        // The Map scatters the proposer's candidate batch.
        let mp = demo
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "mp")
            .expect("map node `mp`");
        match &mp.data {
            WorkflowNodeData::Map {
                items_ref,
                result_var,
                ..
            } => {
                assert_eq!(items_ref, "propose.candidates", "Map itemsRef");
                assert_eq!(result_var, "obs", "Map resultVar");
            }
            other => panic!("`mp` must be a Map, got {other:?}"),
        }
        // The Map body lives inside the Loop body (Map-in-Loop): `mp`'s parent
        // is the loop, `branin`'s parent is the map.
        assert_eq!(
            mp.parent_id.as_deref(),
            Some("lp"),
            "Map nests in the Loop body"
        );

        // The Loop carries five accumulators, none reserved-named. `budget`
        // captures `input.max_iterations` at enter and is carried constant, so
        // the stop condition reads PARKED loop state (`bo.iteration < bo.budget`)
        // instead of a token-resident `input.max_iterations` — which an
        // AutomatedStep body (the Map's `gather`) strips off the control token.
        let lp = demo
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "lp")
            .expect("loop node `lp`");
        match &lp.data {
            WorkflowNodeData::Loop {
                accumulators,
                loop_condition,
                ..
            } => {
                assert_eq!(accumulators.len(), 5, "loop must carry 5 accumulators");
                assert!(
                    accumulators.iter().all(|a| a.var != "iteration"),
                    "no accumulator may be named `iteration` (reserved)"
                );
                let vars: Vec<&str> = accumulators.iter().map(|a| a.var.as_str()).collect();
                assert_eq!(
                    vars,
                    ["observations", "f_best", "best_a", "best_d", "budget"]
                );
                // Stop condition must read parked loop state, not a token leaf
                // stripped by the AutomatedStep body.
                assert!(
                    loop_condition.contains("bo.budget"),
                    "loop_condition must gate on the parked `bo.budget`, got: {loop_condition}"
                );
            }
            other => panic!("`lp` must be a Loop, got {other:?}"),
        }

        // Python source for the proposer must be loaded (it borrows the loop
        // accumulators by slug, driving read-arc synthesis at compile time).
        let propose = demo
            .files
            .get("propose")
            .expect("propose node must have files");
        assert!(
            propose
                .get("main.py")
                .is_some_and(|s| s.contains("bo.observations")),
            "propose/main.py must be loaded and borrow bo.observations"
        );
    }

    /// The BO loop demo must compile end-to-end through the full AIR path —
    /// exercising Map scatter/gather nested inside a Loop body, the four loop
    /// accumulators (init from the entering `input.*` token, merge from the
    /// `gather.*` body outputs), the `evals[*].obs` collection borrow, and
    /// read-arc synthesis for the Python `bo.*` references. A regression in
    /// any of those layers fails here rather than at seed time.
    #[test]
    fn bo_loop_demo_compiles() {
        use crate::compiler::{compile_to_air_with_options, node_files_inline, CompileOptions};

        let demo = load_demo(&repo_root().join("demos/12-bo-loop")).expect("12-bo-loop must load");

        let files = node_files_inline(&demo.files);
        compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("12-bo-loop must compile (Map-in-Loop + accumulators + collection borrow)");
    }

    /// Phase 4 of the BO arc (`12a-bo-catalog-trigger`): a catalog Trigger
    /// node fronting a slim single-pass re-fit body. AIR compilation skips
    /// Trigger nodes (the dispatcher owns them pre-compile), so this proves
    /// two things: (1) the `kind: catalog` TriggerSource + its `payloadMapping`
    /// round-trip through the `WorkflowGraph` types the editor/seeder use, and
    /// (2) the Start→propose→End body compiles cleanly through the same AIR
    /// pipeline `publish` uses, with read-arc synthesis for the `start.*`
    /// borrows in the Python proposer.
    #[test]
    fn bo_catalog_trigger_demo_compiles() {
        use crate::compiler::{compile_to_air_with_options, node_files_inline, CompileOptions};
        use crate::models::template::{TriggerSource, WorkflowNodeData};

        let demo = load_demo(&repo_root().join("demos/12a-bo-catalog-trigger"))
            .expect("12a-bo-catalog-trigger must load");
        assert_eq!(demo.metadata.name, "12a · BO Catalog Trigger");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-0000000000c1"
        );

        // The Trigger node parses as a `catalog` source with the expected
        // filter + a non-empty payloadMapping projecting the entry onto the
        // seed token.
        let trig = demo
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "trigger-bo-obs")
            .expect("trigger node `trigger-bo-obs`");
        match &trig.data {
            WorkflowNodeData::Trigger {
                source,
                payload_mapping,
                ..
            } => {
                match source {
                    TriggerSource::Catalog(cat) => {
                        assert!(!cat.backfill, "backfill must be false");
                        let cat_filter = cat
                            .filters
                            .get("category")
                            .expect("filters.category must be present");
                        assert_eq!(
                            cat_filter.get("eq").map(String::as_str),
                            Some("metric"),
                            "category eq filter"
                        );
                        // The semantic `bo_observation` lives in a
                        // user_metadata sentinel — `category` is a closed enum
                        // (one of the 8 ArtifactCategory values), so a real
                        // producer tags `category=metric` plus
                        // `metadata.kind=bo_observation`.
                        let kind_filter = cat
                            .filters
                            .get("user_metadata.kind")
                            .expect("filters.user_metadata.kind must be present");
                        assert_eq!(
                            kind_filter.get("eq").map(String::as_str),
                            Some("bo_observation"),
                            "user_metadata.kind sentinel"
                        );
                    }
                    other => panic!("trigger source must be Catalog, got {other:?}"),
                }
                let targets: Vec<&str> = payload_mapping
                    .iter()
                    .map(|m| m.target_field.as_str())
                    .collect();
                assert_eq!(
                    targets,
                    ["observations", "last_z"],
                    "payloadMapping projects the catalogue entry onto the seed token"
                );
            }
            other => panic!("`trigger-bo-obs` must be a Trigger, got {other:?}"),
        }

        // AIR compilation skips the Trigger node; the Start→propose→End body
        // must compile (read-arc synthesis for the `start.*` Python borrows).
        let files = node_files_inline(&demo.files);
        compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("12a-bo-catalog-trigger body must compile (Trigger skipped, Start→propose→End)");
    }

    /// The 12b producer demo (the catalogue-side half of Phase 4) loads with
    /// the expected identity and its Start→emit→End body compiles through the
    /// same AIR pipeline `publish` uses — including read-arc synthesis for the
    /// `start.a/d/z` slug borrows in the `aithericon.log_artifact` producer.
    /// Guards against a fixture-shape regression (bad graph.json, missing node
    /// file, templateId collision) being caught only at live-seed time.
    #[test]
    fn bo_observation_producer_demo_compiles() {
        use crate::compiler::{compile_to_air_with_options, node_files_inline, CompileOptions};

        let demo = load_demo(&repo_root().join("demos/12b-bo-observation-producer"))
            .expect("12b-bo-observation-producer must load");
        assert_eq!(demo.metadata.name, "12b · BO Observation Producer");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-0000000000c2"
        );

        let files = node_files_inline(&demo.files);
        compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .expect("12b-bo-observation-producer body must compile (Start→emit→End)");
    }

    /// The learning-path demos (`01-` … `06-`) all parse through the same
    /// types the live `/api/v1/templates` consumer expects. A break here
    /// catches a regression in `WorkflowNodeData` shape against the
    /// bundled fixtures before it hits a user editor session.
    ///
    /// The expected templateIds are the stable ones baked into each
    /// demo.json — tests and the seeder both reach for demos by id, so
    /// drift in that column needs to be a deliberate, type-checked break.
    #[test]
    fn learning_path_demos_all_load() {
        let root = repo_root().join("demos");
        for (dir_name, expected_id, expected_name) in [
            (
                "01-hello-world",
                "00000000-0000-0000-0000-000000000011",
                "01 · Hello World",
            ),
            (
                "02-human-form",
                "00000000-0000-0000-0000-000000000012",
                "02 · Human Form",
            ),
            (
                "03-decision-routing",
                "00000000-0000-0000-0000-000000000013",
                "03 · Decision Routing",
            ),
            (
                "04-loop-counter",
                "00000000-0000-0000-0000-000000000014",
                "04 · Loop Counter",
            ),
            (
                "05-parallel-fanout",
                "00000000-0000-0000-0000-000000000015",
                "05 · Parallel Fanout",
            ),
            (
                "06-subworkflow",
                "00000000-0000-0000-0000-000000000016",
                "06 · SubWorkflow (Flow-in-Flow)",
            ),
            (
                "07-ocr-classify-extract",
                "00000000-0000-0000-0000-000000000017",
                "07 · OCR Classify & Extract",
            ),
            (
                "08-failure-handling",
                "00000000-0000-0000-0000-000000000018",
                "08 · Failure Handling",
            ),
            (
                "10-delay-timeout",
                "00000000-0000-0000-0000-000000000021",
                "10 · Delay & Timeout",
            ),
        ] {
            let demo = load_demo(&root.join(dir_name))
                .unwrap_or_else(|e| panic!("{dir_name} must load: {e}"));
            assert_eq!(
                demo.metadata.template_id, expected_id,
                "{dir_name} templateId"
            );
            assert_eq!(demo.metadata.name, expected_name, "{dir_name} name");
        }
    }

    /// Every numbered learning-path demo (except 06-subworkflow, which
    /// resolves a child at publish time and so can't be compiled through
    /// the in-process `compile_to_air` path) must compile cleanly through
    /// the same AIR pipeline `/api/v1/templates/{id}/publish` uses. A break
    /// here means the demo would seed but fail at publish time with a
    /// stack of compile errors — the seeder logs and continues, which is
    /// silent enough that this test is what catches it.
    #[test]
    fn learning_path_demos_compile_to_air() {
        use crate::compiler::{compile_to_air, node_files_inline};

        let root = repo_root().join("demos");
        for dir_name in [
            "01-hello-world",
            "02-human-form",
            "03-decision-routing",
            "04-loop-counter",
            "05-parallel-fanout",
            "08-failure-handling",
            "10-delay-timeout",
        ] {
            let demo = load_demo(&root.join(dir_name))
                .unwrap_or_else(|e| panic!("{dir_name} must load: {e}"));
            let files = node_files_inline(&demo.files);
            let air = compile_to_air(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
            )
            .unwrap_or_else(|e| panic!("{dir_name} must compile to AIR: {e:?}"));
            // Sanity: serialized AIR must contain at least one transition
            // — rules out a graph that deserialized into an empty net.
            assert!(
                air.to_string().contains("\"transitions\""),
                "{dir_name} AIR must declare transitions"
            );
        }
    }

    /// `07-ocr-classify-extract` exercises the LLM + Kreuzberg upstream-ref
    /// borrow plumbing on real bundled fixtures. The Kreuzberg `file` field
    /// references `{{ start.document }}` (path-site, File kind →
    /// StoragePath staging); the LLM `prompt` references
    /// `{{ extract_text.content }}` (content-site → Raw staging; `content`
    /// is kreuzberg's native ExtractionResult key — no remap). Both must
    /// rewrite to the executor-resolver shape (`{{input_path:…}}` /
    /// `{{input:…}}`) and emit corresponding `job_inputs.push` snippets in
    /// the prepare-transition Rhai source. A break here means the LLM/
    /// Kreuzberg borrow phase regressed on real graphs — the focused unit
    /// tests in `compile.rs` would still pass but the demo would die at
    /// publish.
    #[test]
    fn ocr_classify_extract_demo_loads_and_compiles_with_borrows() {
        use crate::compiler::{
            compile_to_air_with_options, node_files_inline, CompileArtifacts, CompileOptions,
        };

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("07-ocr-classify-extract"))
            .expect("07-ocr-classify-extract must load");
        assert_eq!(
            demo.metadata.template_id,
            "00000000-0000-0000-0000-000000000017"
        );

        let files = node_files_inline(&demo.files);
        let CompileArtifacts {
            air, node_configs, ..
        } = compile_to_air_with_options(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            CompileOptions {
                inline_sources: &demo.files,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("07-ocr-classify-extract must compile to AIR: {e:?}"));

        let air_str = air.to_string();

        // The placeholder rewrites now land in the parked side-channel
        // blob the publish layer uploads, not the AIR. The AIR still
        // carries the borrow-input *staging* (the `job_inputs.push` with
        // the `__pluck(d_<producer>, …)` call) — that's what binds the
        // upstream envelope to the staged file name at runtime.
        let ocr_cfg = node_configs
            .get("extract_text")
            .expect("extract_text node config must be parked")
            .to_string();
        let llm_cfg = node_configs
            .get("classify")
            .expect("classify node config must be parked")
            .to_string();

        // Kreuzberg borrow: file kind producer → StoragePath staging.
        // The compiler rewrites `{{ start.document }}` in the Kreuzberg
        // `file` config to `{{input_path:__borrow_start__document}}` and
        // emits a matching `job_inputs.push` with `storage_path`.
        assert!(
            air_str.contains("__borrow_start__document"),
            "AIR must reference the start.document borrow input by its generated name; got: {air_str}"
        );
        assert!(
            ocr_cfg.contains("input_path:__borrow_start__document"),
            "Kreuzberg file field must be rewritten to {{input_path:…}} in side-channel; got: {ocr_cfg}"
        );
        assert!(
            air_str.contains("storage_path"),
            "File-kind borrow must stage via storage_path; got: {air_str}"
        );

        // LLM borrow: text-kind producer → Raw staging. The compiler
        // rewrites `{{ extract_text.content }}` in the LLM prompt to
        // `{{input:__borrow_extract_text__content}}` and emits a
        // matching `job_inputs.push` with `raw`.
        assert!(
            air_str.contains("__borrow_extract_text__content"),
            "AIR must reference the extract_text.content borrow input by its generated name; got: {air_str}"
        );
        assert!(
            llm_cfg.contains("input:__borrow_extract_text__content"),
            "LLM prompt must be rewritten to {{input:…}} in side-channel; got: {llm_cfg}"
        );

        // Regression guard: the `job_inputs.push` snippets we emit call
        // `__pluck(d_<producer>, …)`. The engine registers `__pluck`
        // natively (see `petri_application::rhai_runtime::register_pluck`),
        // so transitions only need to reference it — they don't have to
        // ship the helper definition. The first cut of the LLM/Kreuzberg
        // borrow phase forgot both ends and shipped AIR that compiled
        // cleanly but threw "Function not found: __pluck" the first
        // time the engine tried to fire it; this assertion locks in
        // the call shape, and the matching execution-level test
        // (`ocr_classify_extract_demo_prepare_transitions_execute`)
        // proves the native registration covers it at runtime.
        assert!(
            air_str.contains("__pluck(d_start"),
            "Kreuzberg borrow must call __pluck on producer envelope; got: {air_str}"
        );
        assert!(
            air_str.contains("__pluck(d_extract_text"),
            "LLM borrow must call __pluck on producer envelope; got: {air_str}"
        );
    }

    /// Execution-level companion to
    /// `ocr_classify_extract_demo_loads_and_compiles_with_borrows`.
    ///
    /// The string-level test above asserts the AIR has the right shape;
    /// this one drives the *actual* Rhai engine the runtime uses
    /// (`petri_application::rhai_runtime::RhaiRuntime`) against the
    /// compiled prepare transitions. Catches:
    ///
    ///   - Missing `__pluck` registration / prelude — the original bug
    ///     ("Function not found: __pluck (map, array)" at fire time)
    ///     was invisible to the compile path but would surface here on
    ///     the first `eval_with_scope`.
    ///   - Scope shape drift — the planner's producer-field hoist logic
    ///     (HumanTask: `.data`, AutomatedStep: `.detail.outputs`, Start:
    ///     top-level) is encoded in the generated Rhai. If that drifts
    ///     out of sync with the parked envelope shape the engine actually
    ///     produces, the eval here returns `()` for the staged value and
    ///     the assertion below catches it.
    ///   - Wrong staging strategy — File-kind producer needs `storage_path`
    ///     with a `.url` pluck; non-File needs `raw` with stringified
    ///     content. The assertion inspects the returned `job_inputs` map
    ///     directly, so a swapped dispatch is caught at the source.
    #[test]
    fn ocr_classify_extract_demo_prepare_transitions_execute() {
        use crate::compiler::compile_to_scenario;
        use crate::compiler::SubWorkflowAir;
        use aithericon_sdk::scenario::TransitionLogic;
        use petri_application::rhai_runtime::RhaiRuntime;
        use rhai::{Dynamic, Map, Scope};
        use serde_json::json;

        let root = repo_root().join("demos");
        let demo = load_demo(&root.join("07-ocr-classify-extract"))
            .expect("07-ocr-classify-extract must load");
        let files = crate::compiler::node_files_inline(&demo.files);
        let scenario = compile_to_scenario(
            &demo.graph,
            &demo.metadata.name,
            demo.metadata.description.as_deref().unwrap_or(""),
            &files,
            &SubWorkflowAir::new(),
        )
        .expect("must compile to scenario");

        // Helper to extract a transition's Rhai source by id-prefix.
        let prepare_source = |id_prefix: &str| -> String {
            scenario
                .transitions
                .iter()
                .find(|t| {
                    t.id == format!("{id_prefix}/prepare")
                        || t.id == format!("t_{id_prefix}_prepare")
                })
                .and_then(|t| match &t.logic {
                    TransitionLogic::Rhai { source } => Some(source.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("prepare transition for {id_prefix} not found"))
        };

        let runtime = RhaiRuntime::new();
        let engine = runtime.engine();

        // ── Kreuzberg prepare: borrows `start.document` (FieldKind::File)
        // → staging strategy is `storage_path` with `__pluck(d_start,
        // ["document", "key"])`.
        let extract_source = prepare_source("extract_text");
        let mut scope = Scope::new();
        scope.push("input", Map::new());
        // Start envelope shape: input form fields land at the TOP LEVEL of
        // the seeded token — no `.data` wrapper (unlike HumanTask). The
        // `parameterize_air` path puts whatever the caller posted directly
        // into `p_{id}_ready`; uploaded files become FileRef maps under
        // their declared field name. The FileRef shape mirrors what the
        // platform's `/api/v1/files/upload/{id}/{node_id}` returns: `key` is
        // the S3 object key (`templates/{id}/blobs/{node_id}/{filename}`)
        // and `url` is the platform-facing HTTP endpoint
        // (`/api/v1/files/<key>`). See `token_shape::
        // valid_uploaded_file_ref_passes` for the exact shape and
        // `app/.../CreateInstanceDialog.svelte` for the frontend
        // construction.
        let d_start: Dynamic = engine
            .parse_json(
                json!({ "document": {
                    "key": "templates/abc/blobs/start/uploaded.pdf",
                    "url": "/api/v1/files/templates/abc/blobs/start/uploaded.pdf",
                    "filename": "uploaded.pdf",
                    "content_type": "application/pdf",
                    "size": 1234
                } })
                .to_string(),
                true,
            )
            .expect("d_start parse")
            .into();
        scope.push_dynamic("d_start", d_start);

        let result: Map = engine
            .eval_with_scope(&mut scope, &extract_source)
            .expect("extract_text/prepare must execute under the synthetic scope");

        let inputs = result
            .get("job")
            .and_then(|v| v.clone().try_cast::<Map>())
            .and_then(|m| m.get("spec").cloned())
            .and_then(|v| v.try_cast::<Map>())
            .and_then(|m| m.get("inputs").cloned())
            .and_then(|v| v.try_cast::<rhai::Array>())
            .expect("job.spec.inputs must be an array");
        let borrow_entry = inputs
            .iter()
            .filter_map(|v| v.clone().try_cast::<Map>())
            .find(|m| {
                m.get("name")
                    .and_then(|v| v.clone().try_cast::<String>())
                    .as_deref()
                    == Some("__borrow_start__document")
            })
            .expect("__borrow_start__document must be staged");
        let source = borrow_entry
            .get("source")
            .and_then(|v| v.clone().try_cast::<Map>())
            .expect("borrow source map");
        assert_eq!(
            source
                .get("type")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("storage_path"),
            "File-kind producer must stage via storage_path; got: {source:?}"
        );
        // The executor's global ArtifactStore concatenates `path` with its
        // configured prefix to address S3 — `path` must be the S3 key
        // (FileRef.key), NOT the platform-facing URL (FileRef.url) which
        // would 404 against S3. Regression for the bug where demo 07's
        // first live run failed with "Failed to deserialize ExecutionSpec:
        // missing field `backend`" because the emitted source carried an
        // empty `storage: {}` AND pointed at `.url` instead of `.key`.
        assert_eq!(
            source
                .get("path")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("templates/abc/blobs/start/uploaded.pdf"),
            "storage_path must resolve to producer's FileRef.key (the S3 \
             object key the executor's global ArtifactStore can download); \
             got: {source:?}"
        );
        // `storage` must be ABSENT (Option<StorageConfig>::None) so the
        // input falls through to the global ArtifactStore. Emitting an
        // empty `{}` here used to deserialize as a partial `StorageConfig`
        // and fail with "missing field `backend`" — see
        // `executor::executor-domain::lib.rs::
        // input_source_storage_path_backward_compat` for the contract.
        assert!(
            !source.contains_key("storage"),
            "storage key must be omitted so the global ArtifactStore is \
             used; emitting {{}} would fail StorageConfig deserialization \
             with 'missing field `backend`'. got: {source:?}"
        );

        // Belt-and-braces: round-trip the entire `job.spec` map through
        // `serde_json` -> `aithericon_executor_domain::ExecutionSpec`. This
        // is the EXACT path
        // `engine::core-engine::executor::client::build_execution_job`
        // walks at runtime, so a regression that re-introduces a partial
        // `storage` map (or any other shape drift) trips here at unit-test
        // speed instead of at first live execution.
        {
            use aithericon_executor_domain::{ExecutionSpec, InputSource};
            let job_value = result
                .get("job")
                .cloned()
                .and_then(|v| v.try_cast::<Map>())
                .expect("job map");
            let spec_dyn = job_value.get("spec").cloned().expect("job.spec");
            let runtime = RhaiRuntime::new();
            let spec_json = runtime
                .dynamic_to_json(spec_dyn)
                .expect("spec dynamic must convert to JSON");
            let spec: ExecutionSpec =
                serde_json::from_value(spec_json.clone()).unwrap_or_else(|e| {
                    panic!(
                        "ExecutionSpec must deserialize from the prepare \
                         transition's emitted spec (regression for the \
                         empty-storage / wrong-path bug): {e}; spec_json = {}",
                        serde_json::to_string_pretty(&spec_json).unwrap()
                    )
                });
            assert_eq!(spec.backend, "kreuzberg");
            let storage_input = spec
                .inputs
                .iter()
                .find(|i| i.name == "__borrow_start__document")
                .expect("borrow input must round-trip");
            match &storage_input.source {
                InputSource::StoragePath { path, storage } => {
                    assert_eq!(
                        path, "templates/abc/blobs/start/uploaded.pdf",
                        "storage_path must carry the S3 key"
                    );
                    assert!(
                        storage.is_none(),
                        "storage must round-trip as None so the global \
                         ArtifactStore handles the download"
                    );
                }
                other => panic!("expected StoragePath, got {other:?}"),
            }
        }

        // ── LLM prepare: borrows `extract_text.content` (FieldKind::Text)
        // → staging strategy is `raw` with `__pluck(d_extract_text,
        // ["detail", "outputs", "content"])`. `content` is kreuzberg's
        // native ExtractionResult key — declarations match 1:1, no remap.
        let classify_source = prepare_source("classify");
        let mut scope = Scope::new();
        scope.push("input", Map::new());
        // AutomatedStep envelope shape: `detail.outputs.<field>`.
        let d_extract_text: Dynamic = engine
            .parse_json(
                json!({ "detail": { "outputs": {
                    "content": "Invoice #INV-001 Amount: $1,234.56 Vendor: ACME"
                } } })
                .to_string(),
                true,
            )
            .expect("d_extract_text parse")
            .into();
        scope.push_dynamic("d_extract_text", d_extract_text);

        let result: Map = engine
            .eval_with_scope(&mut scope, &classify_source)
            .expect("classify/prepare must execute under the synthetic scope");
        let inputs = result
            .get("job")
            .and_then(|v| v.clone().try_cast::<Map>())
            .and_then(|m| m.get("spec").cloned())
            .and_then(|v| v.try_cast::<Map>())
            .and_then(|m| m.get("inputs").cloned())
            .and_then(|v| v.try_cast::<rhai::Array>())
            .expect("job.spec.inputs must be an array");
        let borrow_entry = inputs
            .iter()
            .filter_map(|v| v.clone().try_cast::<Map>())
            .find(|m| {
                m.get("name")
                    .and_then(|v| v.clone().try_cast::<String>())
                    .as_deref()
                    == Some("__borrow_extract_text__content")
            })
            .expect("__borrow_extract_text__content must be staged");
        let source = borrow_entry
            .get("source")
            .and_then(|v| v.clone().try_cast::<Map>())
            .expect("borrow source map");
        // Content sites stage via `inline { value }` — the executor's
        // staging hook (`staging.rs::Inline` arm) serializes `value` as
        // JSON and writes it to a temp file; the `{{input:NAME}}`
        // resolver re-parses that JSON when interpolating. For a string
        // producer field the value is a Rhai-side string and the file
        // round-trips as a JSON-encoded string — exactly the contract
        // the resolver expects for `{{input:NAME}}` inside a prompt.
        assert_eq!(
            source
                .get("type")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("inline"),
            "Content-site (LLM prompt) must stage via inline; got: {source:?}"
        );
        assert_eq!(
            source
                .get("value")
                .and_then(|v| v.clone().try_cast::<String>())
                .as_deref(),
            Some("Invoice #INV-001 Amount: $1,234.56 Vendor: ACME"),
            "inline value must be the plucked text; got: {source:?}"
        );
    }

    /// `06-subworkflow` references `01-hello-world`'s templateId via its
    /// `sub_workflow` node. The seeder publishes demos in lexical order so
    /// `01-` is in place before `06-` resolves — this test pins the
    /// invariant *and* asserts the cross-demo id linkage stays in sync.
    #[test]
    fn subworkflow_demo_references_hello_world_template_id() {
        use crate::models::template::WorkflowNodeData;
        let root = repo_root().join("demos");

        let hello = load_demo(&root.join("01-hello-world")).expect("01-hello-world");
        let sub = load_demo(&root.join("06-subworkflow")).expect("06-subworkflow");

        let call_node = sub
            .graph
            .nodes
            .iter()
            .find(|n| n.id == "call_greet")
            .expect("call_greet sub_workflow node");
        match &call_node.data {
            WorkflowNodeData::SubWorkflow { template_id, .. } => {
                assert_eq!(
                    template_id.to_string(),
                    hello.metadata.template_id,
                    "call_greet must reference 01-hello-world's templateId"
                );
            }
            other => panic!("call_greet must be SubWorkflow, got {other:?}"),
        }

        // And the seeder iterates in lexical order, so `list_demo_dirs`
        // hands them out child-before-parent.
        let dirs = list_demo_dirs(&root).expect("list");
        let hello_idx = dirs
            .iter()
            .position(|p| p.ends_with("01-hello-world"))
            .expect("01-hello-world present");
        let sub_idx = dirs
            .iter()
            .position(|p| p.ends_with("06-subworkflow"))
            .expect("06-subworkflow present");
        assert!(
            hello_idx < sub_idx,
            "child (01-hello-world @ {hello_idx}) must seed before parent (06-subworkflow @ {sub_idx})"
        );
    }

    /// Borrow-phase consolidation regression net. Compiles every bundled
    /// demo that goes through `compile_to_air` (excludes 06-subworkflow,
    /// which needs publish-time child resolution) and dumps the AIR as
    /// canonical sorted JSON to stdout. Run with `--nocapture` before and
    /// after each refactor commit; the two outputs must `diff` clean.
    ///
    /// Coverage rationale: 01-05 + 07 + llm-smoke exercise every borrow
    /// phase touched by the refactor — c2 (Python), c3 (HumanTask),
    /// guards (Decision/Loop), c4/c5 (LLM/Kreuzberg upstream refs).
    ///
    /// This test always passes; it's a stdout artifact, not an assertion.
    /// Wrapped with `BORROW_SNAPSHOT_DUMP=1` so it doesn't blast every
    /// CI run with multi-MB stdout.
    #[test]
    fn dump_all_bundled_demo_air_for_regression() {
        use crate::compiler::{compile_to_air, node_files_inline};

        if std::env::var_os("BORROW_SNAPSHOT_DUMP").is_none() {
            return;
        }

        let root = repo_root().join("demos");
        for dir_name in [
            "01-hello-world",
            "02-human-form",
            "03-decision-routing",
            "04-loop-counter",
            "05-parallel-fanout",
            "07-ocr-classify-extract",
            "08-failure-handling",
            "llm-smoke",
        ] {
            let demo = load_demo(&root.join(dir_name))
                .unwrap_or_else(|e| panic!("{dir_name} must load: {e}"));
            let files = node_files_inline(&demo.files);
            let air = compile_to_air(
                &demo.graph,
                &demo.metadata.name,
                demo.metadata.description.as_deref().unwrap_or(""),
                &files,
            )
            .unwrap_or_else(|e| panic!("{dir_name} must compile to AIR: {e:?}"));

            // serde_json::Value with the `preserve_order` feature OFF (the
            // default) sorts BTreeMap-style on serialization — keys are
            // canonical. `to_string_pretty` is stable across runs.
            let canonical = serde_json::to_string_pretty(&air).expect("AIR must serialize");
            println!("=== {dir_name} ===");
            println!("{canonical}");
            println!("=== /{dir_name} ===");
        }
    }

    /// `demos/<demo>/tests/<name>.json` round-trips into `LoadedDemo.tests`,
    /// preserves all five fields verbatim (JSONB sidecars are stored as-is —
    /// the runner does its own validation at run time), and stays empty for
    /// demos that carry no `tests/` directory.
    #[test]
    fn hello_world_demo_carries_bundled_test() {
        let demo =
            load_demo(&repo_root().join("demos/01-hello-world")).expect("01-hello-world must load");
        assert_eq!(demo.tests.len(), 1, "expected the hello-alice fixture");
        let test = &demo.tests[0];
        assert_eq!(test.name, "hello-alice");
        assert!(test.enabled);
        // start_tokens passes through as raw JSONB.
        let st = test.start_tokens.as_array().expect("array");
        assert_eq!(st[0]["token"]["name"], "Alice");
        // The interpolation assertion is the whole point of the bundled
        // test — round-trip the literal so a regression in the loader
        // (e.g. accidental field rename) breaks here, not at runtime.
        let assertions = test.assertions.as_array().expect("array");
        assert_eq!(assertions[0]["path"], "result.value.greeting");
        assert_eq!(assertions[0]["op"], "eq");
        assert_eq!(assertions[0]["value"], "Hello, {{ start.name }}!");
    }

    #[test]
    fn demos_without_tests_dir_yield_empty_tests_vec() {
        // 07-ocr-classify-extract has no tests/ directory — must not error,
        // must return an empty Vec rather than e.g. `None`.
        let demo = load_demo(&repo_root().join("demos/07-ocr-classify-extract"))
            .expect("07-ocr-classify-extract must load");
        assert!(demo.tests.is_empty());
    }

    #[test]
    fn title_from_slug_titlecases_kebab() {
        assert_eq!(title_from_slug("online-clinic"), "Online Clinic");
        assert_eq!(title_from_slug("basics"), "Basics");
        assert_eq!(title_from_slug("control-flow"), "Control Flow");
        // empty / dangling separators don't produce blank words
        assert_eq!(title_from_slug("a--b-"), "A B");
        assert_eq!(title_from_slug(""), "");
    }

    #[test]
    fn known_demo_categories_resolve_to_curated_copy() {
        let c = category_meta("streaming").expect("streaming is a known category");
        assert_eq!(c.display_name, "Streaming");
        assert!(!c.description.is_empty());
        // an unknown segment has no curated entry (caller falls back to a
        // title-cased slug) — proves the fallback path is reachable.
        assert!(category_meta("not-a-real-category").is_none());
    }

    #[test]
    fn every_seeded_public_demo_declares_a_known_folder() {
        // Guards the demo.json ↔ DEMO_CATEGORIES contract: every public demo
        // must name a curated category (so the seeded tree stays tidy), and
        // private children must NOT (they're never filed). Catches a typo in a
        // new demo's `folder` slug at test time, not at seed time.
        let root = repo_root().join("demos");
        for entry in std::fs::read_dir(&root).expect("demos dir") {
            let dir = entry.expect("dir entry").path();
            if !dir.join("demo.json").exists() {
                continue; // assets/, resources/, … support dirs
            }
            let demo = load_demo(&dir).expect("demo must load");
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let is_private = demo.metadata.visibility.as_deref() == Some("private");
            match (&demo.metadata.folder, is_private) {
                (Some(folder), false) => {
                    let leaf = folder.rsplit('/').next().unwrap();
                    assert!(
                        category_meta(leaf).is_some(),
                        "demo {name}: folder `{folder}` leaf `{leaf}` is not a known DEMO_CATEGORIES slug"
                    );
                }
                (None, false) => panic!("public demo {name} declares no `folder`"),
                (_, true) => { /* private children are never filed; folder optional */ }
            }
        }
    }
}
