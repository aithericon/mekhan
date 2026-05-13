//! Async Worker Component Example
//!
//! Demonstrates the SDK's Component system by building reusable async workers.
//! Shows how multiple instances of the same component get unique prefixed IDs.
//!
//! Run with: `cargo run --example async_worker`
//! Deploy to engine: `cargo run --example async_worker -- --deploy`

use aithericon_sdk::prelude::*;

// === Token Types ===

#[token]
struct Job {
    id: String,
    payload: String,
}

#[token]
struct JobResult {
    job_id: String,
    output: String,
}

#[token]
struct JobError {
    job_id: String,
    reason: String,
}

// === Component Configuration ===

/// An async worker component that processes jobs with retry support.
///
/// This is the "Integrated Circuit" pattern - a reusable subnet that
/// encapsulates common async processing logic with configurable behavior.
struct AsyncWorker {
    name: String,
    image: String,
    retry_limit: u32,
}

impl AsyncWorker {
    fn new(name: &str, image: &str) -> Self {
        Self {
            name: name.to_string(),
            image: image.to_string(),
            retry_limit: 3,
        }
    }

    fn with_retries(mut self, limit: u32) -> Self {
        self.retry_limit = limit;
        self
    }
}

// === Component Outputs ===

/// The places exposed by an AsyncWorker to the outer scope.
struct WorkerOutputs {
    success: PlaceHandle<JobResult>,
    failure: PlaceHandle<JobError>,
}

// === Component Implementation ===

impl Component for AsyncWorker {
    type Input = PlaceHandle<Job>;
    type Output = WorkerOutputs;

    fn name(&self) -> String {
        self.name.clone()
    }

    fn instantiate(self, ctx: &mut Context, input_job: Self::Input) -> Self::Output {
        // Internal places (IDs are auto-prefixed: "transcode_1/outbox", etc.)
        let outbox = ctx.state::<Job>("outbox", "Outbox");
        let success = ctx.state::<JobResult>("success", "Success");
        let failure = ctx.state::<JobError>("failure", "Failure");

        // Prepare: Accept job and prepare for processing
        ctx.transition("prepare", "Prepare")
            .auto_input("job", &input_job)
            .auto_output("req", &outbox)
            .logic(
                r#"#{
                req: #{ id: job.id, payload: job.payload }
            }"#,
            );

        // Process: Simulate async processing and produce result
        // In a real system, this would be connected to a mock adapter
        ctx.transition("process", "Process")
            .auto_input("req", &outbox)
            .auto_output("ok", &success)
            .auto_output("fail", &failure)
            .logic(format!(
                r#"
                // Simulated processing for image: {}
                // Retry limit: {}
                if req.payload != "fail" {{
                    #{{ ok: #{{ job_id: req.id, output: "processed:" + req.payload }} }}
                }} else {{
                    #{{ fail: #{{ job_id: req.id, reason: "Processing failed" }} }}
                }}
            "#,
                self.image, self.retry_limit
            ));

        WorkerOutputs { success, failure }
    }
}

// === Workflow Definition ===

fn definition(ctx: &mut Context) {
    // === External Places (outside components) ===

    // Two separate queues to demonstrate multiple component instances
    let video_queue = ctx.state::<Job>("video-queue", "Video Queue");
    let audio_queue = ctx.state::<Job>("audio-queue", "Audio Queue");
    let archive = ctx.state::<JobResult>("archive", "Archive");
    let errors = ctx.state::<JobError>("errors", "Error Log");

    // === Initial Tokens ===

    ctx.seed(
        &video_queue,
        vec![
            Job {
                id: "v1".into(),
                payload: "video1.mp4".into(),
            },
            Job {
                id: "v2".into(),
                payload: "video2.mp4".into(),
            },
        ],
    );

    ctx.seed(
        &audio_queue,
        vec![
            Job {
                id: "a1".into(),
                payload: "audio1.wav".into(),
            },
            Job {
                id: "a2".into(),
                payload: "fail".into(), // This one will fail
            },
        ],
    );

    // === Component Instantiation ===

    // First worker: Transcode video
    // Internal IDs: "transcode_1/outbox", "transcode_1/success", etc.
    let transcode = ctx.use_component(AsyncWorker::new("Transcode", "ffmpeg:latest"), video_queue);

    // Second worker: Convert audio (same component type, different instance)
    // Internal IDs: "convert_2/outbox", "convert_2/success", etc.
    let convert = ctx.use_component(
        AsyncWorker::new("Convert", "ffmpeg:latest").with_retries(5),
        audio_queue,
    );

    // === Final Wiring ===

    // Archive successful transcodes
    ctx.transition("archive_video", "Archive Video")
        .auto_input("result", &transcode.success)
        .auto_output("done", &archive)
        .logic(r#"#{ done: result }"#);

    // Archive successful conversions
    ctx.transition("archive_audio", "Archive Audio")
        .auto_input("result", &convert.success)
        .auto_output("done", &archive)
        .logic(r#"#{ done: result }"#);

    // Log errors from transcode stage
    ctx.transition("log_transcode_error", "Log Transcode Error")
        .auto_input("err", &transcode.failure)
        .auto_output("logged", &errors)
        .logic(r#"#{ logged: #{ job_id: err.job_id, reason: "Transcode: " + err.reason } }"#);

    // Log errors from convert stage
    ctx.transition("log_convert_error", "Log Convert Error")
        .auto_input("err", &convert.failure)
        .auto_output("logged", &errors)
        .logic(r#"#{ logged: #{ job_id: err.job_id, reason: "Convert: " + err.reason } }"#);
}

fn main() {
    aithericon_sdk::run(
        "async-workers",
        "Component demo with chained async workers",
        definition,
    );
}
