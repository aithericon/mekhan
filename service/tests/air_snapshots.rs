//! AIR golden-output snapshots for the bundled demo workflows.
//!
//! Goal: pin the compiled AIR for each demo so a refactor in
//! `compiler/borrow/`, `compiler/lower.rs`, etc. can be proven a no-op by
//! `cargo test --test air_snapshots`. When a change is INTENDED (new
//! variant landed, new field), re-run with `UPDATE_SNAPSHOTS=1` and
//! review the diff in the commit.
//!
//! Hand-rolled (no `insta` dep) — the assertion is a single helper that
//! diffs pretty-printed JSON against `tests/snapshots/air/<demo>.json`.
//! Set `UPDATE_SNAPSHOTS=1` to (re)write the golden files.
//!
//! `06-subworkflow` and `09-agent-tool-loop` are skipped: both reference a
//! child template by id that the bare `compile_to_air` entry-point can't
//! resolve (the publish handler runs the resolver) — 09's `lookup_order`
//! tool is a SubWorkflow. The snapshot would just be the
//! `SubWorkflowChildMissing` error string, which adds no signal.

use aithericon_executor_domain::InputSource;
use mekhan_service::compiler::compile_to_air;
use mekhan_service::demos::{load_demo, LoadedDemo};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Matches `compiler::lower::NodeFiles` (module is private), kept inline
/// here so the snapshot test doesn't force a visibility widen.
type NodeFiles = HashMap<String, HashMap<String, InputSource>>;

/// Demos whose AIR is reproducible by the bare `compile_to_air` entry
/// (no resource resolution, no child-template lookup, no S3 staging).
const SNAPSHOT_DEMOS: &[&str] = &[
    "01-hello-world",
    "02-human-form",
    "03-decision-routing",
    "04-loop-counter",
    "05-parallel-fanout",
    // 06-subworkflow: needs publish-time child resolution; intentionally skipped.
    "07-ocr-classify-extract",
    "08-failure-handling",
    "10-delay-timeout",
    "11-http-call",
    "12-bo-loop",
    // 13-resource-pool: exercises the M3 resource-pool claim lowering
    // (claim/grant/register/release wrapping an inline Python body). The
    // golden AIR pins the leak-prevention wiring — every body exit arcs to
    // p_render_release_out — against compiler refactors.
    "13-resource-pool",
];

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `service/`; demos live at the repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("air")
}

/// Convert the demo loader's `HashMap<node_id, HashMap<filename, String>>` to
/// the `NodeFiles` shape the compiler wants — every source file becomes
/// `InputSource::Raw`. Mirrors what `POST /templates` does for inline content.
fn to_node_files(demo: &LoadedDemo) -> NodeFiles {
    demo.files
        .iter()
        .map(|(node_id, files)| {
            let inner = files
                .iter()
                .map(|(fname, content)| {
                    (
                        fname.clone(),
                        InputSource::Raw {
                            content: content.clone(),
                        },
                    )
                })
                .collect::<HashMap<_, _>>();
            (node_id.clone(), inner)
        })
        .collect()
}

/// Compile a demo to AIR JSON. Hard-fails if the demo doesn't compile —
/// the snapshot suite is a regression net for green demos only.
fn compile_demo(name: &str) -> Value {
    let dir = repo_root().join("demos").join(name);
    let demo = load_demo(&dir).unwrap_or_else(|e| panic!("load demo {name}: {e}"));
    let files = to_node_files(&demo);
    compile_to_air(&demo.graph, &demo.metadata.name, "", &files)
        .unwrap_or_else(|e| panic!("compile demo {name}: {e}"))
}

/// Stable-order normalize: sort top-level array fields (places, transitions,
/// groups) by `id` so HashMap iteration order in the compiler can't make
/// snapshots flap. Recurses into objects to catch any nested arrays we add
/// later (today these three are the only ones the AIR JSON exposes).
fn normalize(value: &mut Value) {
    if let Value::Object(map) = value {
        for (k, v) in map.iter_mut() {
            normalize(v);
            if matches!(k.as_str(), "places" | "transitions" | "groups") {
                if let Value::Array(arr) = v {
                    arr.sort_by(|a, b| {
                        let ak = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let bk = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        ak.cmp(bk)
                    });
                }
            }
        }
    } else if let Value::Array(arr) = value {
        for v in arr.iter_mut() {
            normalize(v);
        }
    }
}

/// Assert the AIR matches `snapshots/air/<name>.json`. With
/// `UPDATE_SNAPSHOTS=1`, write/overwrite the golden instead of comparing.
fn assert_snapshot(name: &str, mut air: Value) {
    normalize(&mut air);
    let actual = serde_json::to_string_pretty(&air).expect("serialize AIR");
    let path: PathBuf = snapshot_dir().join(format!("{name}.json"));

    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::create_dir_all(snapshot_dir()).expect("create snapshot dir");
        std::fs::write(&path, &actual).unwrap_or_else(|e| panic!("write {path:?}: {e}"));
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {path:?} — run `UPDATE_SNAPSHOTS=1 cargo test \
             --test air_snapshots {name}` to seed it"
        )
    });

    if expected != actual {
        // Write a `.new` sidecar to make diffing painless without re-running.
        let new_path: PathBuf = path.with_extension("json.new");
        std::fs::write(&new_path, &actual).ok();
        let first_diff_line = first_diff_line_number(&expected, &actual);
        panic!(
            "AIR snapshot drift for {name} (first diff at line {first_diff_line}).\n  \
             expected: {path:?}\n  actual:   {new_path:?}\n  \
             diff with: `diff -u {} {}`\n  \
             accept with: `UPDATE_SNAPSHOTS=1 cargo test --test air_snapshots {name}`",
            path.display(),
            new_path.display(),
        );
    }
}

fn first_diff_line_number(a: &str, b: &str) -> usize {
    a.lines()
        .zip(b.lines())
        .position(|(x, y)| x != y)
        .map(|i| i + 1)
        .unwrap_or_else(|| a.lines().count().min(b.lines().count()) + 1)
}

fn run(name: &str) {
    let air = compile_demo(name);
    assert_snapshot(name, air);
}

#[test]
fn snapshot_01_hello_world() {
    run("01-hello-world");
}

#[test]
fn snapshot_02_human_form() {
    run("02-human-form");
}

#[test]
fn snapshot_03_decision_routing() {
    run("03-decision-routing");
}

#[test]
fn snapshot_04_loop_counter() {
    run("04-loop-counter");
}

#[test]
fn snapshot_05_parallel_fanout() {
    run("05-parallel-fanout");
}

#[test]
fn snapshot_07_ocr_classify_extract() {
    run("07-ocr-classify-extract");
}

#[test]
fn snapshot_08_failure_handling() {
    run("08-failure-handling");
}

#[test]
fn snapshot_10_delay_timeout() {
    run("10-delay-timeout");
}

#[test]
fn snapshot_11_http_call() {
    run("11-http-call");
}

#[test]
fn snapshot_12_bo_loop() {
    run("12-bo-loop");
}

#[test]
fn snapshot_13_resource_pool() {
    run("13-resource-pool");
}

/// Catch-all: if a demo is added to the repo and someone forgets to wire
/// a snapshot test, fail loudly. Comparison against the curated list above
/// rather than the disk so we can intentionally exclude (e.g. subworkflow
/// demos that need publish-time resolution) without the test going green
/// from silent omission.
#[test]
fn every_numbered_demo_has_a_snapshot_test_or_is_documented_skip() {
    let demos_dir = repo_root().join("demos");
    let mut numbered: Vec<String> = std::fs::read_dir(&demos_dir)
        .expect("read demos dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // Numbered demos use NN-name. Strip-prefix isolates the convention
            // from the unnumbered ones (`invoice-processing`, `llm-smoke`,
            // ...) that this suite doesn't claim to cover.
            if name.chars().take(2).all(|c| c.is_ascii_digit())
                && name.chars().nth(2) == Some('-')
            {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    numbered.sort();

    let covered: std::collections::HashSet<&str> = SNAPSHOT_DEMOS.iter().copied().collect();
    // Both skipped for the same reason: they reference a child template by id
    // that the bare `compile_to_air` entry-point can't resolve (the publish
    // handler runs the resolver). 09's `lookup_order` tool is a SubWorkflow
    // (the 08a-order-lookup child), so it joins 06 here.
    let documented_skip: std::collections::HashSet<&str> =
        ["06-subworkflow", "09-agent-tool-loop"].into_iter().collect();

    for d in &numbered {
        let s = d.as_str();
        assert!(
            covered.contains(s) || documented_skip.contains(s),
            "demo `{s}` exists on disk but is neither in SNAPSHOT_DEMOS nor in the \
             documented_skip list — add a `snapshot_{}` test or extend the skip list \
             with a reason.",
            s.replace('-', "_"),
        );
    }
}

