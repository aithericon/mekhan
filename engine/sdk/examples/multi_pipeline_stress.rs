//! Multi-Pipeline Stress Test - Performance & Complexity Benchmark
//!
//! Stress test demonstrating:
//! - Multiple parallel processing pipelines (Video, Audio, Image, Document)
//! - Shared resource pools (GPUs shared between video/image, CPUs for audio/document)
//! - Multi-stage processing (Ingest → Validate → Process → Compress → Archive)
//! - High token counts for throughput testing
//!
//! Run with: `cargo run --example multi_pipeline_stress`
//! Deploy to engine: `cargo run --example multi_pipeline_stress -- --deploy`

use aithericon_sdk::prelude::*;

// ============================================================================
// Shared Token Types
// ============================================================================

#[token]
struct Job {
    id: String,
    job_type: String,
    priority: i64,
    max_retries: i64,
    retries: i64,
    payload_size_mb: i64,
}

#[token]
struct GpuWorker {
    id: String,
    vram_gb: i64,
}

#[token]
struct CpuWorker {
    id: String,
    cores: i64,
}

#[token]
struct IngestCtx {
    job_id: String,
    job_type: String,
    worker_id: String,
    max_retries: i64,
    retries: i64,
}

#[token]
struct Validated {
    job_id: String,
    job_type: String,
    worker_id: String,
    max_retries: i64,
    retries: i64,
    quality_score: i64,
}

#[token]
struct Processed {
    job_id: String,
    job_type: String,
    worker_id: String,
    output_size_mb: i64,
}

#[token]
struct Compressed {
    job_id: String,
    job_type: String,
    compression_ratio: i64,
}

#[token]
struct Archived {
    job_id: String,
    job_type: String,
    archive_path: String,
    timestamp: String,
}

#[token]
struct Failed {
    job_id: String,
    job_type: String,
    stage: String,
    error: String,
}

// Signal tokens
#[token]
struct ValidatedSignal {
    correlation_id: String,
    quality_score: i64,
}

#[token]
struct ValidationErrorSignal {
    correlation_id: String,
    error: String,
}

#[token]
struct ProcessedSignal {
    correlation_id: String,
    output_size_mb: i64,
}

#[token]
struct ProcessErrorSignal {
    correlation_id: String,
    error: String,
    fatal: bool,
}

#[token]
struct CompressedSignal {
    correlation_id: String,
    compression_ratio: i64,
}

#[token]
struct CompressErrorSignal {
    correlation_id: String,
    error: String,
}

// ============================================================================
// Stage 1: Ingest (GPU and CPU variants)
// ============================================================================

#[step("t_ingest_gpu", "1. Ingest (GPU)")]
fn ingest_gpu(job: Job, worker: GpuWorker) -> IngestCtx {
    IngestCtx {
        job_id: job.id,
        job_type: job.job_type,
        worker_id: worker.id,
        max_retries: job.max_retries,
        retries: job.retries,
    }
}

#[step("t_ingest_cpu", "1. Ingest (CPU)")]
fn ingest_cpu(job: Job, worker: CpuWorker) -> IngestCtx {
    IngestCtx {
        job_id: job.id,
        job_type: job.job_type,
        worker_id: worker.id,
        max_retries: job.max_retries,
        retries: job.retries,
    }
}

// ============================================================================
// Stage 2: Validate (GPU variants)
// ============================================================================

#[step("t_validate_ok_gpu", "2a. Validate OK (GPU)")]
#[guard("ingest.job_id == sig.correlation_id && sig.quality_score >= 70")]
fn validate_ok_gpu(ingest: IngestCtx, sig: ValidatedSignal) -> Validated {
    Validated {
        job_id: ingest.job_id,
        job_type: ingest.job_type,
        worker_id: ingest.worker_id,
        max_retries: ingest.max_retries,
        retries: ingest.retries,
        quality_score: sig.quality_score,
    }
}

#[step("t_validate_retry_gpu", "2b. Low Quality Retry (GPU)")]
#[guard("ingest.job_id == sig.correlation_id && sig.quality_score < 70 && ingest.retries < ingest.max_retries")]
fn validate_retry_gpu(
    ingest: IngestCtx,
    sig: ValidatedSignal,
    job: Target<Job>,
    worker: Target<GpuWorker>,
) {
    r#"#{ job: #{ id: ingest.job_id, job_type: ingest.job_type, priority: 0, max_retries: ingest.max_retries, retries: ingest.retries + 1, payload_size_mb: 0 }, worker: #{ id: ingest.worker_id, vram_gb: 0 } }"#
}

#[step("t_validate_exhaust_gpu", "2c. Validate Exhausted (GPU)")]
#[guard("ingest.job_id == sig.correlation_id && sig.quality_score < 70 && ingest.retries >= ingest.max_retries")]
fn validate_exhaust_gpu(
    ingest: IngestCtx,
    sig: ValidatedSignal,
    fail: Target<Failed>,
    worker: Target<GpuWorker>,
) {
    r#"#{ fail: #{ job_id: ingest.job_id, job_type: ingest.job_type, stage: "validate", error: "Quality too low" }, worker: #{ id: ingest.worker_id, vram_gb: 0 } }"#
}

#[step("t_validate_error_gpu", "2d. Validate Error (GPU)")]
#[guard("ingest.job_id == sig.correlation_id")]
fn validate_error_gpu(
    ingest: IngestCtx,
    sig: ValidationErrorSignal,
    fail: Target<Failed>,
    worker: Target<GpuWorker>,
) {
    r#"#{ fail: #{ job_id: ingest.job_id, job_type: ingest.job_type, stage: "validate", error: sig.error }, worker: #{ id: ingest.worker_id, vram_gb: 0 } }"#
}

// ============================================================================
// Stage 2: Validate (CPU variants)
// ============================================================================

#[step("t_validate_ok_cpu", "2a. Validate OK (CPU)")]
#[guard("ingest.job_id == sig.correlation_id && sig.quality_score >= 70")]
fn validate_ok_cpu(ingest: IngestCtx, sig: ValidatedSignal) -> Validated {
    Validated {
        job_id: ingest.job_id,
        job_type: ingest.job_type,
        worker_id: ingest.worker_id,
        max_retries: ingest.max_retries,
        retries: ingest.retries,
        quality_score: sig.quality_score,
    }
}

#[step("t_validate_retry_cpu", "2b. Low Quality Retry (CPU)")]
#[guard("ingest.job_id == sig.correlation_id && sig.quality_score < 70 && ingest.retries < ingest.max_retries")]
fn validate_retry_cpu(
    ingest: IngestCtx,
    sig: ValidatedSignal,
    job: Target<Job>,
    worker: Target<CpuWorker>,
) {
    r#"#{ job: #{ id: ingest.job_id, job_type: ingest.job_type, priority: 0, max_retries: ingest.max_retries, retries: ingest.retries + 1, payload_size_mb: 0 }, worker: #{ id: ingest.worker_id, cores: 0 } }"#
}

#[step("t_validate_exhaust_cpu", "2c. Validate Exhausted (CPU)")]
#[guard("ingest.job_id == sig.correlation_id && sig.quality_score < 70 && ingest.retries >= ingest.max_retries")]
fn validate_exhaust_cpu(
    ingest: IngestCtx,
    sig: ValidatedSignal,
    fail: Target<Failed>,
    worker: Target<CpuWorker>,
) {
    r#"#{ fail: #{ job_id: ingest.job_id, job_type: ingest.job_type, stage: "validate", error: "Quality too low" }, worker: #{ id: ingest.worker_id, cores: 0 } }"#
}

#[step("t_validate_error_cpu", "2d. Validate Error (CPU)")]
#[guard("ingest.job_id == sig.correlation_id")]
fn validate_error_cpu(
    ingest: IngestCtx,
    sig: ValidationErrorSignal,
    fail: Target<Failed>,
    worker: Target<CpuWorker>,
) {
    r#"#{ fail: #{ job_id: ingest.job_id, job_type: ingest.job_type, stage: "validate", error: sig.error }, worker: #{ id: ingest.worker_id, cores: 0 } }"#
}

// ============================================================================
// Stage 3: Process (GPU variants)
// ============================================================================

#[step("t_process_ok_gpu", "3a. Process OK (GPU)")]
#[guard("validated.job_id == sig.correlation_id")]
fn process_ok_gpu(
    validated: Validated,
    sig: ProcessedSignal,
    processed: Target<Processed>,
    worker: Target<GpuWorker>,
) {
    r#"#{ processed: #{ job_id: validated.job_id, job_type: validated.job_type, worker_id: validated.worker_id, output_size_mb: sig.output_size_mb }, worker: #{ id: validated.worker_id, vram_gb: 0 } }"#
}

#[step("t_process_retry_gpu", "3b. Process Retry (GPU)")]
#[guard("validated.job_id == sig.correlation_id && !sig.fatal && validated.retries < validated.max_retries")]
fn process_retry_gpu(
    validated: Validated,
    sig: ProcessErrorSignal,
    job: Target<Job>,
    worker: Target<GpuWorker>,
) {
    r#"#{ job: #{ id: validated.job_id, job_type: validated.job_type, priority: 0, max_retries: validated.max_retries, retries: validated.retries + 1, payload_size_mb: 0 }, worker: #{ id: validated.worker_id, vram_gb: 0 } }"#
}

#[step("t_process_fail_gpu", "3c. Process Fail (GPU)")]
#[guard("validated.job_id == sig.correlation_id && (sig.fatal || validated.retries >= validated.max_retries)")]
fn process_fail_gpu(
    validated: Validated,
    sig: ProcessErrorSignal,
    fail: Target<Failed>,
    worker: Target<GpuWorker>,
) {
    r#"#{ fail: #{ job_id: validated.job_id, job_type: validated.job_type, stage: "process", error: sig.error }, worker: #{ id: validated.worker_id, vram_gb: 0 } }"#
}

// ============================================================================
// Stage 3: Process (CPU variants)
// ============================================================================

#[step("t_process_ok_cpu", "3a. Process OK (CPU)")]
#[guard("validated.job_id == sig.correlation_id")]
fn process_ok_cpu(
    validated: Validated,
    sig: ProcessedSignal,
    processed: Target<Processed>,
    worker: Target<CpuWorker>,
) {
    r#"#{ processed: #{ job_id: validated.job_id, job_type: validated.job_type, worker_id: validated.worker_id, output_size_mb: sig.output_size_mb }, worker: #{ id: validated.worker_id, cores: 0 } }"#
}

#[step("t_process_retry_cpu", "3b. Process Retry (CPU)")]
#[guard("validated.job_id == sig.correlation_id && !sig.fatal && validated.retries < validated.max_retries")]
fn process_retry_cpu(
    validated: Validated,
    sig: ProcessErrorSignal,
    job: Target<Job>,
    worker: Target<CpuWorker>,
) {
    r#"#{ job: #{ id: validated.job_id, job_type: validated.job_type, priority: 0, max_retries: validated.max_retries, retries: validated.retries + 1, payload_size_mb: 0 }, worker: #{ id: validated.worker_id, cores: 0 } }"#
}

#[step("t_process_fail_cpu", "3c. Process Fail (CPU)")]
#[guard("validated.job_id == sig.correlation_id && (sig.fatal || validated.retries >= validated.max_retries)")]
fn process_fail_cpu(
    validated: Validated,
    sig: ProcessErrorSignal,
    fail: Target<Failed>,
    worker: Target<CpuWorker>,
) {
    r#"#{ fail: #{ job_id: validated.job_id, job_type: validated.job_type, stage: "process", error: sig.error }, worker: #{ id: validated.worker_id, cores: 0 } }"#
}

// ============================================================================
// Stage 4: Compress
// ============================================================================

#[step("t_compress_ok", "4a. Compress OK")]
#[guard("processed.job_id == sig.correlation_id")]
fn compress_ok(processed: Processed, sig: CompressedSignal) -> Compressed {
    Compressed {
        job_id: processed.job_id,
        job_type: processed.job_type,
        compression_ratio: sig.compression_ratio,
    }
}

#[step("t_compress_fail", "4b. Compress Fail")]
#[guard("processed.job_id == sig.correlation_id")]
fn compress_fail(processed: Processed, sig: CompressErrorSignal, fail: Target<Failed>) {
    r#"#{ fail: #{ job_id: processed.job_id, job_type: processed.job_type, stage: "compress", error: sig.error } }"#
}

// ============================================================================
// Stage 5: Archive
// ============================================================================

#[step("t_archive", "5. Archive")]
fn archive_step(compressed: Compressed) -> Archived {
    Archived {
        job_id: compressed.job_id,
        job_type: compressed.job_type,
        archive_path: compressed.job_id, // Use job_id as path placeholder
        timestamp: compressed.job_type,  // Use job_type as timestamp placeholder
    }
}

// ============================================================================
// GPU Pipeline Component
// ============================================================================

struct GpuPipelineInputs {
    job_queue: PlaceHandle<Job>,
    gpu_pool: PlaceHandle<GpuWorker>,
}

struct PipelineOutputs {
    archived: PlaceHandle<Archived>,
    failed: PlaceHandle<Failed>,
}

struct GpuMediaPipeline {
    name: String,
    validate_latency: u64,
    process_latency: u64,
    compress_latency: u64,
}

impl GpuMediaPipeline {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            validate_latency: 200,
            process_latency: 1000,
            compress_latency: 300,
        }
    }
}

impl Component for GpuMediaPipeline {
    type Input = GpuPipelineInputs;
    type Output = PipelineOutputs;

    fn name(&self) -> String {
        self.name.clone()
    }

    fn instantiate(self, ctx: &mut Context, inputs: Self::Input) -> Self::Output {
        let GpuPipelineInputs {
            job_queue,
            gpu_pool,
        } = inputs;

        // Signal places
        let sig_validated = ctx.signal::<ValidatedSignal>("sig_validated", "Sig: Validated");
        let sig_validate_error =
            ctx.signal::<ValidationErrorSignal>("sig_validate_error", "Sig: Validate Error");
        let sig_processed = ctx.signal::<ProcessedSignal>("sig_processed", "Sig: Processed");
        let sig_process_error =
            ctx.signal::<ProcessErrorSignal>("sig_process_error", "Sig: Process Error");
        let sig_compressed = ctx.signal::<CompressedSignal>("sig_compressed", "Sig: Compressed");
        let sig_compress_error =
            ctx.signal::<CompressErrorSignal>("sig_compress_error", "Sig: Compress Error");

        // Terminal places
        let archived = ctx.state::<Archived>("archived", "Archived");
        let failed = ctx.state::<Failed>("failed", "Failed");

        // Intermediate place for processed
        let processed = ctx.state::<Processed>("processed", "Processed");

        // === Stage 1: Ingest ===
        let ingesting = ingest_gpu(ctx, &job_queue, &gpu_pool);

        // === Stage 2: Validate ===
        let validated = validate_ok_gpu(ctx, &ingesting, &sig_validated);
        validate_retry_gpu(ctx, &ingesting, &sig_validated, &job_queue, &gpu_pool);
        validate_exhaust_gpu(ctx, &ingesting, &sig_validated, &failed, &gpu_pool);
        validate_error_gpu(ctx, &ingesting, &sig_validate_error, &failed, &gpu_pool);

        // === Stage 3: Process ===
        process_ok_gpu(ctx, &validated, &sig_processed, &processed, &gpu_pool);
        process_retry_gpu(ctx, &validated, &sig_process_error, &job_queue, &gpu_pool);
        process_fail_gpu(ctx, &validated, &sig_process_error, &failed, &gpu_pool);

        // === Stage 4: Compress ===
        let compressed = compress_ok(ctx, &processed, &sig_compressed);
        compress_fail(ctx, &processed, &sig_compress_error, &failed);

        // === Stage 5: Archive ===
        let archive_place = archive_step(ctx, &compressed);

        // Wire archive to terminal
        ctx.transition("finalize", "Finalize")
            .auto_input("a", &archive_place)
            .auto_output("done", &archived)
            .logic(r#"#{ done: a }"#);

        // === Mock Adapters ===
        ctx.mock_adapter(
            &ingesting,
            format!("{} Validator", self.name),
            self.validate_latency,
            format!(
                r#"
                let r = random();
                let quality = 50 + (r * 100.0) % 50;
                if r < 0.9 {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, quality_score: quality }} }}
                }} else {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "Validation unavailable" }} }}
                }}
                "#,
                sig_validated.id(),
                sig_validate_error.id()
            ),
        );

        ctx.mock_adapter(
            &validated,
            format!("{} Processor", self.name),
            self.process_latency,
            format!(
                r#"
                let r = random();
                if r < 0.75 {{
                    let size = 100 + (r * 1000.0) % 500;
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, output_size_mb: size }} }}
                }} else if r < 0.95 {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "GPU memory exhausted", fatal: false }} }}
                }} else {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "GPU failure", fatal: true }} }}
                }}
                "#,
                sig_processed.id(),
                sig_process_error.id(),
                sig_process_error.id()
            ),
        );

        ctx.mock_adapter(
            &processed,
            format!("{} Compressor", self.name),
            self.compress_latency,
            format!(
                r#"
                let r = random();
                if r < 0.95 {{
                    let ratio = 2 + (r * 10.0) % 8;
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, compression_ratio: ratio }} }}
                }} else {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "Codec error" }} }}
                }}
                "#,
                sig_compressed.id(),
                sig_compress_error.id()
            ),
        );

        PipelineOutputs { archived, failed }
    }
}

// ============================================================================
// CPU Pipeline Component
// ============================================================================

struct CpuPipelineInputs {
    job_queue: PlaceHandle<Job>,
    cpu_pool: PlaceHandle<CpuWorker>,
}

struct CpuMediaPipeline {
    name: String,
    validate_latency: u64,
    process_latency: u64,
    compress_latency: u64,
}

impl CpuMediaPipeline {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            validate_latency: 300,
            process_latency: 1500,
            compress_latency: 500,
        }
    }
}

impl Component for CpuMediaPipeline {
    type Input = CpuPipelineInputs;
    type Output = PipelineOutputs;

    fn name(&self) -> String {
        self.name.clone()
    }

    fn instantiate(self, ctx: &mut Context, inputs: Self::Input) -> Self::Output {
        let CpuPipelineInputs {
            job_queue,
            cpu_pool,
        } = inputs;

        // Signal places
        let sig_validated = ctx.signal::<ValidatedSignal>("sig_validated", "Sig: Validated");
        let sig_validate_error =
            ctx.signal::<ValidationErrorSignal>("sig_validate_error", "Sig: Validate Error");
        let sig_processed = ctx.signal::<ProcessedSignal>("sig_processed", "Sig: Processed");
        let sig_process_error =
            ctx.signal::<ProcessErrorSignal>("sig_process_error", "Sig: Process Error");
        let sig_compressed = ctx.signal::<CompressedSignal>("sig_compressed", "Sig: Compressed");
        let sig_compress_error =
            ctx.signal::<CompressErrorSignal>("sig_compress_error", "Sig: Compress Error");

        // Terminal places
        let archived = ctx.state::<Archived>("archived", "Archived");
        let failed = ctx.state::<Failed>("failed", "Failed");

        // Intermediate place for processed
        let processed = ctx.state::<Processed>("processed", "Processed");

        // === Stage 1: Ingest ===
        let ingesting = ingest_cpu(ctx, &job_queue, &cpu_pool);

        // === Stage 2: Validate ===
        let validated = validate_ok_cpu(ctx, &ingesting, &sig_validated);
        validate_retry_cpu(ctx, &ingesting, &sig_validated, &job_queue, &cpu_pool);
        validate_exhaust_cpu(ctx, &ingesting, &sig_validated, &failed, &cpu_pool);
        validate_error_cpu(ctx, &ingesting, &sig_validate_error, &failed, &cpu_pool);

        // === Stage 3: Process ===
        process_ok_cpu(ctx, &validated, &sig_processed, &processed, &cpu_pool);
        process_retry_cpu(ctx, &validated, &sig_process_error, &job_queue, &cpu_pool);
        process_fail_cpu(ctx, &validated, &sig_process_error, &failed, &cpu_pool);

        // === Stage 4: Compress ===
        let compressed = compress_ok(ctx, &processed, &sig_compressed);
        compress_fail(ctx, &processed, &sig_compress_error, &failed);

        // === Stage 5: Archive ===
        let archive_place = archive_step(ctx, &compressed);

        // Wire archive to terminal
        ctx.transition("finalize", "Finalize")
            .auto_input("a", &archive_place)
            .auto_output("done", &archived)
            .logic(r#"#{ done: a }"#);

        // === Mock Adapters ===
        ctx.mock_adapter(
            &ingesting,
            format!("{} Validator", self.name),
            self.validate_latency,
            format!(
                r#"
                let r = random();
                let quality = 50 + (r * 100.0) % 50;
                if r < 0.9 {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, quality_score: quality }} }}
                }} else {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "Validation unavailable" }} }}
                }}
                "#,
                sig_validated.id(),
                sig_validate_error.id()
            ),
        );

        ctx.mock_adapter(
            &validated,
            format!("{} Processor", self.name),
            self.process_latency,
            format!(
                r#"
                let r = random();
                if r < 0.75 {{
                    let size = 50 + (r * 500.0) % 200;
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, output_size_mb: size }} }}
                }} else if r < 0.95 {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "CPU overload", fatal: false }} }}
                }} else {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "Kernel panic", fatal: true }} }}
                }}
                "#,
                sig_processed.id(),
                sig_process_error.id(),
                sig_process_error.id()
            ),
        );

        ctx.mock_adapter(
            &processed,
            format!("{} Compressor", self.name),
            self.compress_latency,
            format!(
                r#"
                let r = random();
                if r < 0.95 {{
                    let ratio = 2 + (r * 10.0) % 8;
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, compression_ratio: ratio }} }}
                }} else {{
                    #{{ target_place: "{}", data: #{{ correlation_id: token.job_id, error: "Codec error" }} }}
                }}
                "#,
                sig_compressed.id(),
                sig_compress_error.id()
            ),
        );

        PipelineOutputs { archived, failed }
    }
}

// ============================================================================
// Seed Helpers
// ============================================================================

fn seed_jobs(prefix: &str, job_type: &str, count: usize, max_retries: i64) -> Vec<Job> {
    (0..count)
        .map(|i| Job {
            id: format!("{}-{}", prefix, i + 1),
            job_type: job_type.to_string(),
            priority: if i % 5 == 0 { 1 } else { 0 },
            max_retries,
            retries: 0,
            payload_size_mb: 100 + (i as i64 * 50) % 500,
        })
        .collect()
}

fn seed_gpus(count: usize) -> Vec<GpuWorker> {
    (0..count)
        .map(|i| GpuWorker {
            id: format!("gpu-{}", i + 1),
            vram_gb: 8 + (i as i64 * 8) % 32,
        })
        .collect()
}

fn seed_cpus(count: usize) -> Vec<CpuWorker> {
    (0..count)
        .map(|i| CpuWorker {
            id: format!("cpu-{}", i + 1),
            cores: 4 + (i as i64 * 4) % 32,
        })
        .collect()
}

// ============================================================================
// Workflow Definition
// ============================================================================

fn definition(ctx: &mut Context) {
    // Shared Resource Pools
    let gpu_pool = ctx.state::<GpuWorker>("gpu_pool", "GPU Pool");
    let cpu_pool = ctx.state::<CpuWorker>("cpu_pool", "CPU Pool");

    ctx.seed(&gpu_pool, seed_gpus(4));
    ctx.seed(&cpu_pool, seed_cpus(8));

    // Job Queues
    let video_queue = ctx.state::<Job>("video_queue", "Video Queue");
    let image_queue = ctx.state::<Job>("image_queue", "Image Queue");
    let audio_queue = ctx.state::<Job>("audio_queue", "Audio Queue");
    let doc_queue = ctx.state::<Job>("doc_queue", "Document Queue");

    // Seed jobs (305 total)
    ctx.seed(&video_queue, seed_jobs("vid", "video", 50, 2));
    ctx.seed(&image_queue, seed_jobs("img", "image", 100, 3));
    ctx.seed(&audio_queue, seed_jobs("aud", "audio", 75, 2));
    ctx.seed(&doc_queue, seed_jobs("doc", "document", 80, 1));

    // Shared Terminals
    let master_archive = ctx.state::<Archived>("master_archive", "Master Archive");
    let error_log = ctx.state::<Failed>("error_log", "Error Log");

    // Instantiate GPU Pipelines (Video + Image share GPU pool)
    let video_pipeline = ctx.use_component(
        GpuMediaPipeline::new("Video"),
        GpuPipelineInputs {
            job_queue: video_queue,
            gpu_pool: gpu_pool.clone(),
        },
    );

    let image_pipeline = ctx.use_component(
        GpuMediaPipeline::new("Image"),
        GpuPipelineInputs {
            job_queue: image_queue,
            gpu_pool: gpu_pool.clone(),
        },
    );

    // Instantiate CPU Pipelines (Audio + Document share CPU pool)
    let audio_pipeline = ctx.use_component(
        CpuMediaPipeline::new("Audio"),
        CpuPipelineInputs {
            job_queue: audio_queue,
            cpu_pool: cpu_pool.clone(),
        },
    );

    let doc_pipeline = ctx.use_component(
        CpuMediaPipeline::new("Document"),
        CpuPipelineInputs {
            job_queue: doc_queue,
            cpu_pool: cpu_pool.clone(),
        },
    );

    // Wire outputs to shared terminals
    ctx.transition("archive_video", "Archive Video")
        .auto_input("a", &video_pipeline.archived)
        .auto_output("out", &master_archive)
        .logic(r#"#{ out: #{ job_id: "video:" + a.job_id, job_type: a.job_type, archive_path: a.archive_path, timestamp: a.timestamp } }"#);

    ctx.transition("archive_image", "Archive Image")
        .auto_input("a", &image_pipeline.archived)
        .auto_output("out", &master_archive)
        .logic(r#"#{ out: #{ job_id: "image:" + a.job_id, job_type: a.job_type, archive_path: a.archive_path, timestamp: a.timestamp } }"#);

    ctx.transition("archive_audio", "Archive Audio")
        .auto_input("a", &audio_pipeline.archived)
        .auto_output("out", &master_archive)
        .logic(r#"#{ out: #{ job_id: "audio:" + a.job_id, job_type: a.job_type, archive_path: a.archive_path, timestamp: a.timestamp } }"#);

    ctx.transition("archive_doc", "Archive Document")
        .auto_input("a", &doc_pipeline.archived)
        .auto_output("out", &master_archive)
        .logic(r#"#{ out: #{ job_id: "doc:" + a.job_id, job_type: a.job_type, archive_path: a.archive_path, timestamp: a.timestamp } }"#);

    ctx.transition("log_video_error", "Log Video Error")
        .auto_input("f", &video_pipeline.failed)
        .auto_output("out", &error_log)
        .logic(r#"#{ out: #{ job_id: "video:" + f.job_id, job_type: f.job_type, stage: f.stage, error: f.error } }"#);

    ctx.transition("log_image_error", "Log Image Error")
        .auto_input("f", &image_pipeline.failed)
        .auto_output("out", &error_log)
        .logic(r#"#{ out: #{ job_id: "image:" + f.job_id, job_type: f.job_type, stage: f.stage, error: f.error } }"#);

    ctx.transition("log_audio_error", "Log Audio Error")
        .auto_input("f", &audio_pipeline.failed)
        .auto_output("out", &error_log)
        .logic(r#"#{ out: #{ job_id: "audio:" + f.job_id, job_type: f.job_type, stage: f.stage, error: f.error } }"#);

    ctx.transition("log_doc_error", "Log Document Error")
        .auto_input("f", &doc_pipeline.failed)
        .auto_output("out", &error_log)
        .logic(r#"#{ out: #{ job_id: "doc:" + f.job_id, job_type: f.job_type, stage: f.stage, error: f.error } }"#);
}

fn main() {
    aithericon_sdk::run(
        "multi-pipeline-stress",
        "Multi-pipeline stress test: 4 pipelines, shared GPU/CPU pools, 305 jobs, 5-stage processing.",
        definition,
    );
}
