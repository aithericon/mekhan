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
    // 08-failure-handling: wired error handle (handled Result::Err) — not snapshotted.
    // 08c-unwired-failure: the panic-on-unconnected-failure fixture. Its golden pins
    // the compiled shape — the exhausted transition `throw`s (permanent ScriptError ->
    // NetFailed) and NO dead-end `p_risky_step_error` place exists.
    "08c-unwired-failure",
    "08-failure-handling",
    "10-delay-timeout",
    "11-http-call",
    "12-bo-loop",
    // 13-resource-pool: a plain executor-dispatch Python step. The shared-pool
    // admission showcase moved off the well-known global onto a named
    // `concurrency_limit` RESOURCE bound via
    // `deploymentModel: { mode: "executor", capacity: { alias } }` (consolidation
    // pivot) — and the demo seeder provisions templates, not resources, so the
    // seeded demo is plain executor dispatch. The pooled-lowering AIR shape is
    // pinned by `compiler_e2e`'s aliased-pool tests instead; the live pool
    // showcase is an R5 dogfood step.
    "13-resource-pool",
    "13-dynamic-form",
    "14-streaming-output",
    "15-stream-python-body",
    "17-stream-map",
    "18-stream-pipeline",
    // 36-audio-transcribe: Start File borrow → binary DATA channel → consumer.
    // Compiles offline (no Python run / model download needed at compile time);
    // RUNNING it is live-only (faster-whisper), documented in demo.json.
    "36-audio-transcribe",
    // 42-live-audio-stream: Start File borrow → binary DATA channel produced but
    // UNCONSUMED (no consumer edge) — the UI taps it live via ?follow=1. Pins
    // that an unwired data OUT channel compiles; running it is live-only (paced).
    "42-live-audio-stream",
    // 43-lossy-frame-stream: producer → consumer over a `nats-latest` (lossy
    // core-NATS) DATA channel — pins that the per-channel `transport` tag lowers
    // into the manifest; running it (live-only) proves the executor dispatches
    // the lossy adapter off the descriptor with zero SDK change.
    "43-lossy-frame-stream",
    // 44-durable-blob-stream: producer → consumer over an `s3` (durable
    // object-store) DATA channel — a different transport SHAPE (key/value, not
    // pub/sub). Pins that the `s3` transport tag lowers into the manifest;
    // running it (live-only) proves the executor dispatches the object-store
    // adapter off the descriptor, lossless + replayable, with zero SDK change.
    "44-durable-blob-stream",
    // 45-live-fmp4-stream: producer (PyAV-muxed fragmented MP4) → validator over
    // a default-transport DATA channel whose element content_type is
    // `audio/mp4;codecs="mp4a.40.2"`. Pins that an audio/mp4 element type lowers
    // cleanly; the point is the PRESENTATION-side render-adapter dispatch (the UI
    // routes this channel to the MSE player off the content_type), live-verified
    // in the browser, not in the AIR snapshot.
    "45-live-fmp4-stream",
    // 46-live-video-stream: the VIDEO sibling of 45 — producer (PyAV/libx264
    // H.264 fragmented MP4) → validator over a default-transport DATA channel
    // whose element content_type is `video/mp4;codecs="avc1.42E01E"`. Same
    // lowering as 45; the point is the PRESENTATION dispatch routing this to the
    // `<video>` + MSE path, live-verified in the browser.
    "46-live-video-stream",
    // 47-stream-object-detection: the AI capstone — a real clip flows over a
    // binary DATA channel (Start File borrow → stream step re-muxes to fragmented
    // MP4), a detector step CONSUMES that data stream and EMITS a CONTROL stream
    // of recognized objects on `detections`, and a `join: gather` edge folds them
    // in `summary`. Pins that consume-data-channel + emit-control-channel + gather
    // all lower together in one node graph; the YOLO inference itself is live-only.
    "47-stream-object-detection",
    // 48-live-camera-detection: the live-viz loop closed — a live source (webcam
    // index / RTSP-HTTP URL / file path) streams per-frame JPEGs over a lossy
    // `nats-latest` DATA channel, a YOLO26 detector CONSUMES it per frame and
    // produces BOTH a CONTROL stream of detections (gathered in `summary`) AND a
    // box-annotated `image/jpeg` DATA channel the UI plays as a live MJPEG feed.
    // Pins per-frame data-in + dual data/control-out (one with no consumer, UI
    // tapped) + gather lowering together; inference + camera are live-only.
    "48-live-camera-detection",
    // 50-legacy-crawl-register: file-ops `crawl` (streaming `{path,size,mtime}`
    // batches over a CONTROL channel) → consumer-edge `join: gather` fold →
    // Python register step. Pins that a file_ops node emits a control channel a
    // gather consumer folds, plus the compiler's source-scan of the register
    // step's `start.*` / `fold.files` borrows. Self-contained (no external
    // resource/asset), so bare `compile_to_air` resolves it; RUNNING it is
    // live-only (seeds a temp NAS, POSTs to the inventory API).
    "50-legacy-crawl-register",
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
fn snapshot_08c_unwired_failure() {
    run("08c-unwired-failure");
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

#[test]
fn snapshot_13_dynamic_form() {
    run("13-dynamic-form");
}

#[test]
fn snapshot_14_streaming_output() {
    run("14-streaming-output");
}

#[test]
fn snapshot_15_stream_python_body() {
    run("15-stream-python-body");
}

#[test]
fn snapshot_17_stream_map() {
    run("17-stream-map");
}

#[test]
fn snapshot_18_stream_pipeline() {
    run("18-stream-pipeline");
}

#[test]
fn snapshot_36_audio_transcribe() {
    run("36-audio-transcribe");
}

#[test]
fn snapshot_42_live_audio_stream() {
    run("42-live-audio-stream");
}

#[test]
fn snapshot_43_lossy_frame_stream() {
    run("43-lossy-frame-stream");
}

#[test]
fn snapshot_44_durable_blob_stream() {
    run("44-durable-blob-stream");
}

#[test]
fn snapshot_45_live_fmp4_stream() {
    run("45-live-fmp4-stream");
}

#[test]
fn snapshot_46_live_video_stream() {
    run("46-live-video-stream");
}

#[test]
fn snapshot_47_stream_object_detection() {
    run("47-stream-object-detection");
}

#[test]
fn snapshot_48_live_camera_detection() {
    run("48-live-camera-detection");
}

#[test]
fn snapshot_50_legacy_crawl_register() {
    run("50-legacy-crawl-register");
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
            if name.chars().take(2).all(|c| c.is_ascii_digit()) && name.chars().nth(2) == Some('-')
            {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    numbered.sort();

    let covered: std::collections::HashSet<&str> = SNAPSHOT_DEMOS.iter().copied().collect();
    // Skipped for the same reason: each references a child template or resource
    // by id that the bare `compile_to_air` entry-point can't resolve (the
    // publish handler runs the resolver). 09's `lookup_order` tool is a
    // SubWorkflow (the 08a-order-lookup child), so it joins 06; 11-http-call's
    // `http` backend references an http resource the resolver would supply;
    // 12a-bo-catalog-trigger carries a Trigger node whose catalogue wiring is
    // resolved at publish time — same class of "needs the publish handler".
    // 16-leased-gpu's LeaseScope binds the `nomad_dc` datacenter resource, whose
    // backing net the bare `compile_to_air` has no KnownResources to resolve; its
    // compiled shape is pinned by `compiler_e2e`'s lease-scope tests and the live
    // seed (publish path) proves it compiles.
    // 19-postgres-node binds the `demo_pg` postgres resource via ConfigOverlay —
    // bare `compile_to_air` has no KnownResources to resolve it; its compile is
    // pinned by `demos::tests::postgres_node_demo_loads_and_compiles_with_resource`
    // (which passes a known `demo_pg`) and the live seed proves the publish path.
    // 21-asset-consume / 22-asset-ref / 23-resource-ref / 24-asset-python-ref are the
    // named-global demos: each references a scope-visible asset/resource by ref-key
    // that only the publish handler's `discover_named_globals` resolver can supply
    // (bare `compile_to_air` has an empty KnownGlobals, so the asset staging /
    // object-asset constant-inline / resource public-field constant-inline can't
    // resolve). Their compile + inline behavior is proven live by the publish path +
    // `mekhan test`.
    let documented_skip: std::collections::HashSet<&str> = [
        "06-subworkflow",
        "09-agent-tool-loop",
        "11-http-call",
        "12a-bo-catalog-trigger",
        "16-leased-gpu",
        "19-postgres-node",
        "20-loki-query",
        "21-asset-consume",
        "22-asset-ref",
        "23-resource-ref",
        "24-asset-python-ref",
        // 25-prometheus-query references a `prometheus` resource (like 20-loki),
        // resolved by the publish handler; bare `compile_to_air` has no
        // KnownResources for it. Proven live by the publish path + `mekhan test`.
        "25-prometheus-query",
        // 26-runner-pool / 27-runner-xrd target a `runner_group` resource
        // (`lab_fleet`) via `deploymentModel.capacity.alias`; lowering the pooled
        // body needs that resource resolved (publish handler / KnownResources),
        // which bare `compile_to_air` lacks. 27 additionally references the
        // `xrd` capability registry for its placement Requirements. The pooled-
        // lowering AIR shape is pinned by `compiler_e2e`'s aliased-pool tests;
        // the presence-pool routing + capability match are proven live by
        // `just dev runner-up` + `mekhan test`.
        "26-runner-pool",
        "27-runner-xrd",
        // 28-turtle-drive (ROS) is the same shape as 27: its ros AutomatedSteps
        // target the `ros_fleet` runner_group via `deploymentModel.capacity.alias`
        // and carry a `ros` capability Requirement — neither resolvable by bare
        // `compile_to_air` (empty KnownResources/capability registry). The
        // presence-pool routing + capability match + the ROS backend ops are
        // proven live by `just dev ros-up` + `mekhan test`.
        "28-turtle-drive",
        // 29-turtle-rotate (ROS action + streaming channel) is the same shape as
        // 28: its ros AutomatedSteps target the `ros_fleet` runner_group via
        // `deploymentModel.capacity.alias` and carry a `ros` capability
        // Requirement — neither resolvable by bare `compile_to_air` (empty
        // KnownResources/capability registry). The presence-pool routing +
        // capability match + the `send_action_goal` feedback path (now a docs/25
        // Control/Scatter channel, drained by a Python `fold` reducer) are proven
        // live by `just dev ros-up` + `mekhan test`.
        "29-turtle-rotate",
        // 30-xarm-joint-move (ROS / xArm) is the same shape as 28/29: its ros
        // AutomatedSteps target the `xarm_fleet` runner_group via
        // `deploymentModel.capacity.alias` and carry a `ros` capability
        // Requirement — neither resolvable by bare `compile_to_air` (empty
        // KnownResources/capability registry). Proven live by
        // `just dev xarm-up` + `mekhan test`.
        "30-xarm-joint-move",
        // 31-xarm-wave (ROS / xArm) is the same shape as 30: its ros
        // AutomatedSteps target the `xarm_fleet` runner_group via
        // `deploymentModel.capacity.alias` and carry a `ros` capability
        // Requirement — neither resolvable by bare `compile_to_air` (empty
        // KnownResources/capability registry). Proven live by
        // `just dev xarm-up` + `mekhan test`.
        "31-xarm-wave",
        // 32-xarm-trajectory-stream (ROS / xArm action + streaming channel) is
        // the same shape as 29/31: its ros AutomatedStep targets the
        // `xarm_fleet` runner_group via `deploymentModel.capacity.alias` and
        // carries a `ros` capability Requirement — neither resolvable by bare
        // `compile_to_air`. The structured `FollowJointTrajectory_Feedback`
        // stream is a docs/25 Control/Scatter channel drained by a Python `fold`
        // reducer. Proven live by `just dev xarm-up` + `mekhan test`.
        "32-xarm-trajectory-stream",
        // 33-xarm-pose-plan-execute (ROS / xArm MoveIt, docs/26-27) targets the
        // `xarm_fleet` runner_group via `deploymentModel.capacity.alias` and
        // carries a `ros` capability Requirement — not resolvable by bare
        // `compile_to_air`. Proven live by `just dev xarm-up` + `mekhan test`.
        "33-xarm-pose-plan-execute",
        // 34-xarm-scene-plan (ROS / xArm MoveIt Path C S2, docs/27) — same
        // `xarm_fleet` + `ros` Requirement shape as 33; adds a collision object
        // to the persistent planning scene then plans around it. Live-only.
        "34-xarm-scene-plan",
        // 35-xarm-grasp-release (ROS / xArm MoveIt Path C S3, docs/27) — same
        // `xarm_fleet` + `ros` Requirement shape as 33; atomic grasp/release
        // (gripper actuation + scene attach/detach). Live-only.
        "35-xarm-grasp-release",
        // 41-prepare-cell (ROS / xArm MoveIt Path C S4, docs/27) — same
        // `xarm_fleet` + `ros` Requirement shape as 33; stages the planning
        // scene / cell for sample handling. Renumbered off 36 (taken by
        // 36-audio-transcribe on main). Live-only.
        "41-prepare-cell",
        // 37-pick (ROS / xArm MoveIt Path C S4, docs/27) — same `xarm_fleet` +
        // `ros` Requirement shape as 33; pick SubWorkflow building block.
        // Live-only.
        "37-pick",
        // 38-place (ROS / xArm MoveIt Path C S4, docs/27) — same `xarm_fleet` +
        // `ros` Requirement shape as 33; place SubWorkflow building block.
        // Live-only.
        "38-place",
        // 39-swap (ROS / xArm MoveIt Path C S4, docs/27) — same `xarm_fleet` +
        // `ros` Requirement shape as 33; swap SubWorkflow composing pick/place.
        // Live-only.
        "39-swap",
        // 40-sample-handling (ROS / xArm MoveIt Path C S4, docs/27) — same
        // `xarm_fleet` + `ros` Requirement shape as 33; the north-star
        // sample-handling workflow composing the pick/place/swap SubWorkflows.
        // Live-only.
        "40-sample-handling",
        // 37-internal-pool-agent (model-pool P1, docs/28/29) — Agent step calling
        // a self-hosted LLM via the internal model pool (Ollama/vLLM-backed). Needs
        // a live model-pool + inference backend, not a deterministic AIR snapshot.
        // Live-only.
        "37-internal-pool-agent",
        // 49-xarm-twin (robot-twin, live 3D URDF on an edge) — references a
        // `robot_description` asset by ref-key (`;model=xarm6`), a named-global
        // only the publish handler's resolver can supply (same class as the
        // asset-ref demos 21-24); bare `compile_to_air` has an empty KnownGlobals.
        // Live-only (Threlte/urdf-loader render + playback source).
        "49-xarm-twin",
        // 51-scene-twin (live planning-scene twin — samples + grasped sample on
        // the arm) — same class as 49: ros AutomatedSteps target the `xarm_fleet`
        // runner_group via `deploymentModel.capacity.alias`, carry a `ros`
        // capability Requirement, and reference a `robot_description` asset by
        // ref-key (`;model=`), none resolvable by bare `compile_to_air` (empty
        // KnownResources/KnownGlobals). Live-only (Threlte scene render + MoveIt).
        "51-scene-twin",
        // 53-human-review (human-capacity offer dispatch, docs/33) — its
        // HumanTask binds the `reviewers` `capacity` resource via
        // `data.capacity.alias`, lowered as the pooled offer claim/acquire/
        // register/release scaffold. The alias resolves to `pool-<capacity_id>`
        // only at publish time against a live `resources` row (seeded by
        // `seed_demo_resources` + enrolled by `seed_demo_roster`); bare
        // `compile_to_air` has an empty KnownResources. Same class as the
        // runner-group demos above. Proven live via the Inbox claim flow.
        "53-human-review",
    ]
    .into_iter()
    .collect();

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
