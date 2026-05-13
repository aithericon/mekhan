//! Layer 0: Research Brief Orchestrator — full-feature demo combining all major capabilities.
//!
//! Demonstrates 7 features in a single coherent workflow:
//! 1. **Human UI (data entry)** — researcher enters topic, questions, and sources
//! 2. **Python executor** — gathers research data using Vault-injected API keys
//! 3. **Vault secret injection** — API_KEY and DATA_SOURCE_TOKEN wrapped for executor
//! 4. **File transfer** — copies raw data between two local storage backends
//! 5. **Rig/Ollama LLM** — generates a structured research report via extraction
//! 6. **Human UI (review)** — senior analyst approves or rejects the report
//! 7. **Nomad scheduler** — all jobs dispatched through Nomad (via scheduler-net)
//!
//! ## Architecture: 4-Layer Bridged Nets
//!
//! ```text
//! Layer 0: orchestrator-net   (this net — human tasks + sequencing)
//!          | bridge_out → bridge_in
//! Layer 1: job-net            (job_net --bridged --upstream orchestrator-net)
//!          | bridge_out → bridge_in
//! Layer 2: scheduler-net      (scheduler_net — Nomad dispatch)
//!          | bridge_out → bridge_in
//! Layer 3: executor-net       (executor_net --bridged — executor lifecycle)
//! ```
//!
//! ## Data flow
//!
//! ```text
//! [entry_form] → (request_entry: human_task) → [entry_task] + [sig_entry_response]
//! [entry_task] + [sig_entry_response] → (parse_entry) → [research_params]
//!
//! [research_params] → (prepare_python) → [python_ready]
//! [python_ready] → (dispatch_python) → [to_jobs: bridge_out] + [python_pending]
//! [result_inbox: bridge_in] + [python_pending] → (join_python) → [python_done]
//!
//! [python_done] → (prepare_file) → [file_ready]
//! [file_ready] → (dispatch_file) → [to_jobs] + [file_pending]
//! [result_inbox] + [file_pending] → (join_file) → [file_done]
//!
//! [file_done] → (prepare_llm) → [llm_ready]
//! [llm_ready] → (dispatch_llm) → [to_jobs] + [llm_pending]
//! [result_inbox] + [llm_pending] → (join_llm) → [report_data]
//!
//! [report_data] → (prepare_review) → [review_form]
//! [review_form] → (request_review: human_task) → [review_task] + [sig_review_response]
//! [review_task] + [sig_review_response] → (approve/reject) → [approved] / [rejected]
//!
//! [failure_inbox] + [X_pending] → (fail_X) → [workflow_failed]
//! ```
//!
//! ## Deploy
//!
//! ```bash
//! just demo research-brief
//! ```
//!
//! ## Net ID: `orchestrator-net`

use aithericon_sdk::prelude::*;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Human data entry form (seeded — triggers the first human task).
#[token]
struct EntryForm {
    title: String,
    instructions_mdsvex: String,
    steps: serde_json::Value,
}

/// Assigned entry task (output of human_task effect).
#[token]
struct EntryTask {
    task_id: String,
}

/// Parsed research parameters from human entry.
#[token]
struct ResearchParams {
    topic: String,
    questions: serde_json::Value,
    sources: String,
    priority: String,
}

/// Step ready to be dispatched as a job.
#[token]
struct StepReady {
    job_id: String,
    model_name: String,
    run: i64,
    retries: i64,
    max_retries: i64,
    spec: serde_json::Value,
    context: serde_json::Value,
}

/// Pending step — held while waiting for job-net result.
#[token]
struct StepPending {
    job_id: String,
    phase: String,
    context: serde_json::Value,
}

/// Job dispatched to job-net via bridge.
#[token]
struct JobRequest {
    job_id: String,
    model_name: String,
    run: i64,
    retries: i64,
    max_retries: i64,
    spec: serde_json::Value,
}

/// Result received from job-net.
#[token]
struct StepResult {
    job_id: String,
    model_name: String,
    detail: serde_json::Value,
}

/// Failure received from job-net.
#[token]
struct StepFailure {
    job_id: String,
    model_name: String,
    reason: String,
    retries_exhausted: i64,
}

/// Python data gathering completed.
#[token]
struct PythonDone {
    topic: String,
    raw_data: serde_json::Value,
}

/// File transfer completed.
#[token]
struct FileDone {
    topic: String,
    processed_data: serde_json::Value,
    bytes_transferred: i64,
}

/// LLM report generated.
#[token]
struct ReportData {
    topic: String,
    report: serde_json::Value,
    raw_data: serde_json::Value,
    bytes_transferred: i64,
}

/// Review form token for the human review task.
#[token]
struct ReviewForm {
    title: String,
    instructions_mdsvex: String,
    steps: serde_json::Value,
    topic: String,
    report: serde_json::Value,
}

/// Assigned review task.
#[token]
struct ReviewTask {
    task_id: String,
    topic: String,
    report: serde_json::Value,
}

/// Approved research brief.
#[token]
struct ApprovedReport {
    topic: String,
    report: serde_json::Value,
    reviewer_comments: String,
}

/// Rejected research brief.
#[token]
struct RejectedReport {
    topic: String,
    reason: String,
}

/// Terminal workflow failure.
#[token]
struct WorkflowFailed {
    phase: String,
    job_id: String,
    reason: String,
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // -------------------------------------------------------------------------
    // Places
    // -------------------------------------------------------------------------

    // Human data entry
    let entry_form = ctx.state::<HumanTaskRequest>("entry_form", "Entry Form");
    let entry_task = ctx.state::<HumanTaskAssigned>("entry_task", "Entry Task");
    let sig_entry_response = ctx.signal::<HumanTaskResponse>("sig_entry_response", "Entry Response Signal");
    let research_params = ctx.state::<ResearchParams>("research_params", "Research Parameters");

    // Python phase
    let python_ready = ctx.state::<StepReady>("python_ready", "Python Job Ready");

    // Shared bridge out — ALL job dispatches go to job-net
    let to_jobs = ctx.bridge_out::<JobRequest>("to_jobs", "To Jobs", "job-net", "job_queue");

    // Shared bridge in — results/failures from job-net
    let result_inbox =
        ctx.bridge_in_from::<StepResult>("result_inbox", "Result Inbox", "job-net", "result_outbox");
    let failure_inbox = ctx.bridge_in_from::<StepFailure>(
        "failure_inbox",
        "Failure Inbox",
        "job-net",
        "failure_outbox",
    );

    // Phase pending places
    let python_pending = ctx.state::<StepPending>("python_pending", "Python Pending");
    let file_pending = ctx.state::<StepPending>("file_pending", "File Transfer Pending");
    let llm_pending = ctx.state::<StepPending>("llm_pending", "LLM Pending");

    // Phase done places
    let python_done = ctx.state::<PythonDone>("python_done", "Python Done");
    let file_ready = ctx.state::<StepReady>("file_ready", "File Job Ready");
    let file_done = ctx.state::<FileDone>("file_done", "File Transfer Done");
    let llm_ready = ctx.state::<StepReady>("llm_ready", "LLM Job Ready");
    let report_data = ctx.state::<ReportData>("report_data", "Report Data");

    // Human review
    let review_form = ctx.state::<HumanTaskRequest>("review_form", "Review Form");
    let review_task = ctx.state::<HumanTaskAssigned>("review_task", "Review Task");
    let sig_review_response =
        ctx.signal::<HumanTaskResponse>("sig_review_response", "Review Response Signal");

    // Terminal places
    let approved = ctx.state::<ApprovedReport>("approved", "Approved Reports");
    let rejected = ctx.state::<RejectedReport>("rejected", "Rejected Reports");
    let workflow_failed = ctx.state::<WorkflowFailed>("workflow_failed", "Workflow Failed");
    let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");

    // -------------------------------------------------------------------------
    // Process Lifecycle
    // -------------------------------------------------------------------------

    let (process_done, processes) = ctx.scope("Process Lifecycle", |ctx| {
        let process_inbox = ctx.state::<DynamicToken>("process_inbox", "Process Inbox");
        let processes = ctx.state::<ProcessStarted>("processes", "Active Processes");
        let process_done = ctx.state::<DynamicToken>("process_done", "Process Done");
        let process_completed = ctx.state::<DynamicToken>("process_completed", "Process Completed");

        ctx.seed(&process_inbox, vec![DynamicToken::new(serde_json::json!({}))]);

        ctx.transition("create_process", "Create Process")
            .process_start_to(ProcessStart {
                trigger: &process_inbox,
                process: &processes,
                config: ProcessStartConfig::new("Research Brief")
                    .process_id_prefix("rb-")
                    .human_step("entry", "Data Entry")
                    .step("python", "Data Gathering")
                    .step("file", "File Transfer")
                    .step("llm", "Report Generation")
                    .human_step("review", "Report Review"),
            });

        ctx.transition("complete_process", "Complete Process")
            .process_complete_to(ProcessComplete {
                process: &processes,
                done: &process_done,
                completed: &process_completed,
            });

        (process_done, processes)
    });

    // -------------------------------------------------------------------------
    // Seed: initial entry form
    // -------------------------------------------------------------------------

    ctx.seed(
        &entry_form,
        vec![HumanTaskRequest {
            task_id: None,
            net_id: None,
            org_id: None,
            place: None,
            corr_id: None,
            title: "Research Brief Request".into(),
            instructions_mdsvex: Some("Enter your research parameters. The system will gather data, \
                transfer it for processing, generate an AI report, and route it for review.".into()),
            payload: None,
            response_subject: None,
            process_id: None,
            process_step: None,
            steps: vec![
                TaskStep::new("params", "Research Parameters")
                    .description("Provide the details for your research brief.")
                    .input(
                        TaskField::text("topic", "Research Topic")
                            .required()
                            .placeholder("e.g. Impact of generative AI on enterprise software"),
                    )
                    .input(
                        TaskField::textarea("questions", "Key Questions (one per line)")
                            .required()
                            .placeholder("What are the main use cases?\nWhat are the risks?\nWhat is the market outlook?"),
                    )
                    .input(
                        TaskField::text("sources", "Preferred Sources")
                            .placeholder("e.g. arxiv, pubmed, industry reports"),
                    )
                    .input(
                        TaskField::select("priority", "Priority")
                            .required()
                            .options(&["standard", "high", "urgent"]),
                    ),
            ],
        }],
    );

    // -------------------------------------------------------------------------
    // Phase 1: Human data entry
    // -------------------------------------------------------------------------

    // 1. request_entry — human_task effect creates the form in the UI
    ctx.transition("request_entry", "Request Data Entry")
        .process_step_started("entry")
        .read_input("process", &processes)
        .human_task_to(HumanTaskSubmit {
            task: &entry_form,
            assigned: &entry_task,
            errors: &effect_errors,
            response_signal: &sig_entry_response,
        });

    // 2. parse_entry — join human response with task, extract research params
    ctx.transition("parse_entry", "Parse Entry Response")
        .process_step_completed("entry")
        .read_input("process", &processes)
        .auto_input("task", &entry_task)
        .auto_input("response", &sig_entry_response)
        .guard(r#"response.task_id == task.task_id"#)
        .auto_output("params", &research_params)
        .logic(
            r#"#{
                params: #{
                    topic: response.data.topic,
                    questions: response.data.questions,
                    sources: if response.data.sources == () { "" } else { response.data.sources },
                    priority: response.data.priority
                }
            }"#,
        );

    // -------------------------------------------------------------------------
    // Phase 2: Python data gathering (with Vault secrets)
    // -------------------------------------------------------------------------

    let python_script = include_str!("../../demos/research_brief/gather_data.py");

    // 3. prepare_python — build Python executor spec with embedded script + secret refs
    ctx.transition("prepare_python", "Prepare Python Job")
        .process_step_started("python")
        .read_input("process", &processes)
        .auto_input("params", &research_params)
        .auto_output("ready", &python_ready)
        .logic(format!(
            r#"
            let job_id = "research:python:" + params.topic;
            #{{
                ready: #{{
                    job_id: job_id,
                    model_name: "python-gather",
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                    spec: #{{
                        type: "python",
                        config: #{{
                            script: "gather_data.py",
                            virtualenv: true,
                            sdk: true,
                            env: #{{
                                API_KEY: "{{{{secret:demo/api#key}}}}",
                                DATA_SOURCE_TOKEN: "{{{{secret:demo/datasource#token}}}}"
                            }}
                        }},
                        inputs: [
                            #{{
                                name: "gather_data.py",
                                source: #{{
                                    type: "raw",
                                    content: {script_json}
                                }},
                                required: true
                            }},
                            #{{
                                name: "request.json",
                                source: #{{
                                    type: "inline",
                                    value: #{{
                                        topic: params.topic,
                                        questions: params.questions,
                                        sources: params.sources,
                                        priority: params.priority
                                    }}
                                }},
                                required: true
                            }}
                        ],
                        outputs: [
                            #{{ name: "result", required: true }}
                        ]
                    }},
                    context: #{{ topic: params.topic, questions: params.questions }}
                }}
            }}
        "#,
            script_json = serde_json::to_string(python_script).unwrap()
        ));

    // 4. dispatch_python — bridge to job-net, hold pending
    ctx.transition("dispatch_python", "Dispatch Python Job")
        .auto_input("step", &python_ready)
        .auto_output("req", &to_jobs)
        .auto_output("pending", &python_pending)
        .logic(
            r#"#{
                req: #{
                    job_id: step.job_id,
                    model_name: step.model_name,
                    run: step.run,
                    retries: step.retries,
                    max_retries: step.max_retries,
                    spec: step.spec
                },
                pending: #{
                    job_id: step.job_id,
                    phase: "python",
                    context: step.context
                }
            }"#,
        );

    // 5. join_python — match result with pending on job_id
    ctx.transition("join_python", "Join Python Result")
        .process_step_completed("python")
        .read_input("process", &processes)
        .auto_input("result", &result_inbox)
        .auto_input("pending", &python_pending)
        .correlate("result", "pending", "job_id")
        .auto_output("done", &python_done)
        .logic(
            r#"#{
                done: #{
                    topic: pending.context.topic,
                    raw_data: result.detail
                }
            }"#,
        );

    // -------------------------------------------------------------------------
    // Phase 3: File transfer (two local storage backends)
    // -------------------------------------------------------------------------

    // 6. prepare_file — build file-ops spec for copy between storage backends
    ctx.transition("prepare_file", "Prepare File Transfer Job")
        .process_step_started("file")
        .read_input("process", &processes)
        .auto_input("data", &python_done)
        .auto_output("ready", &file_ready)
        .logic(
            r#"
            let job_id = "research:file:" + data.topic;
            #{
                ready: #{
                    job_id: job_id,
                    model_name: "file-transfer",
                    run: 0,
                    retries: 0,
                    max_retries: 1,
                    spec: #{
                        type: "file_ops",
                        config: #{
                            operation: "copy",
                            source: "raw_data.json",
                            destination: "raw_data.json",
                            source_storage: #{
                                backend: "local",
                                endpoint: "/tmp/research/raw"
                            },
                            destination_storage: #{
                                backend: "s3",
                                endpoint: "http://localhost:9005",
                                bucket: "research",
                                region: "us-east-1",
                                credentials: #{
                                    access_key: "rustfsadmin",
                                    secret_key: "rustfsadmin"
                                }
                            }
                        },
                        inputs: [],
                        outputs: [#{ name: "copied", required: true }]
                    },
                    context: #{ topic: data.topic, raw_data: data.raw_data }
                }
            }
        "#,
        );

    // 7. dispatch_file — bridge to job-net, hold pending
    ctx.transition("dispatch_file", "Dispatch File Transfer Job")
        .auto_input("step", &file_ready)
        .auto_output("req", &to_jobs)
        .auto_output("pending", &file_pending)
        .logic(
            r#"#{
                req: #{
                    job_id: step.job_id,
                    model_name: step.model_name,
                    run: step.run,
                    retries: step.retries,
                    max_retries: step.max_retries,
                    spec: step.spec
                },
                pending: #{
                    job_id: step.job_id,
                    phase: "file",
                    context: step.context
                }
            }"#,
        );

    // 8. join_file — match result with pending
    ctx.transition("join_file", "Join File Transfer Result")
        .process_step_completed("file")
        .read_input("process", &processes)
        .auto_input("result", &result_inbox)
        .auto_input("pending", &file_pending)
        .correlate("result", "pending", "job_id")
        .auto_output("done", &file_done)
        .logic(
            r#"#{
                done: #{
                    topic: pending.context.topic,
                    processed_data: pending.context.raw_data,
                    bytes_transferred: result.detail.outputs.bytes_transferred
                }
            }"#,
        );

    // -------------------------------------------------------------------------
    // Phase 4: LLM report generation (Rig/Ollama)
    // -------------------------------------------------------------------------

    // 9. prepare_llm — build llm spec for structured extraction
    ctx.transition("prepare_llm", "Prepare LLM Report Job")
        .process_step_started("llm")
        .read_input("process", &processes)
        .auto_input("data", &file_done)
        .auto_output("ready", &llm_ready)
        .logic(
            r#"
            let job_id = "research:llm:" + data.topic;
            #{
                ready: #{
                    job_id: job_id,
                    model_name: "llm-report",
                    run: 0,
                    retries: 0,
                    max_retries: 1,
                    spec: #{
                        type: "llm",
                        config: #{
                            provider: "ollama",
                            model: "gpt-oss:20b",
                            base_url: "http://localhost:11434",
                            prompt: "You are a research analyst. Based on the following processed research data, generate a comprehensive research brief.\n\nResearch Topic: " + data.topic + "\n\nProcessed Data:\n" + data.processed_data.to_string(),
                            response_format: #{
                                type: "json_schema",
                                schema: #{
                                    type: "object",
                                    properties: #{
                                        title: #{ type: "string" },
                                        executive_summary: #{ type: "string" },
                                        key_findings: #{ type: "array", items: #{ type: "string" } },
                                        risk_assessment: #{ type: "string" },
                                        recommendations: #{ type: "array", items: #{ type: "string" } },
                                        confidence_score: #{ type: "number" }
                                    },
                                    required: ["title", "executive_summary", "key_findings", "recommendations", "confidence_score"]
                                }
                            }
                        },
                        inputs: [
                            #{
                                name: "input_data.json",
                                source: #{
                                    type: "inline",
                                    value: data.processed_data
                                },
                                required: true
                            }
                        ],
                        outputs: [#{ name: "response", required: true }]
                    },
                    context: #{ topic: data.topic, processed_data: data.processed_data, bytes_transferred: data.bytes_transferred }
                }
            }
        "#,
        );

    // 10. dispatch_llm — bridge to job-net, hold pending
    ctx.transition("dispatch_llm", "Dispatch LLM Report Job")
        .auto_input("step", &llm_ready)
        .auto_output("req", &to_jobs)
        .auto_output("pending", &llm_pending)
        .logic(
            r#"#{
                req: #{
                    job_id: step.job_id,
                    model_name: step.model_name,
                    run: step.run,
                    retries: step.retries,
                    max_retries: step.max_retries,
                    spec: step.spec
                },
                pending: #{
                    job_id: step.job_id,
                    phase: "llm",
                    context: step.context
                }
            }"#,
        );

    // 11. join_llm — match LLM result with pending
    ctx.transition("join_llm", "Join LLM Result")
        .process_step_completed("llm")
        .read_input("process", &processes)
        .auto_input("result", &result_inbox)
        .auto_input("pending", &llm_pending)
        .correlate("result", "pending", "job_id")
        .auto_output("report", &report_data)
        .logic(
            r#"#{
                report: #{
                    topic: pending.context.topic,
                    report: result.detail.outputs.response,
                    raw_data: pending.context.processed_data,
                    bytes_transferred: pending.context.bytes_transferred
                }
            }"#,
        );

    // -------------------------------------------------------------------------
    // Phase 5: Human review
    // -------------------------------------------------------------------------

    // 12. prepare_review — build the review form for the senior analyst
    ctx.transition("prepare_review", "Prepare Review Form")
        .auto_input("data", &report_data)
        .auto_output("form", &review_form)
        .logic(
            r##"
            let r = data.report;

            // Summary block (mdsvex)
            let summary = "**" + r.title + "**\n\n";
            summary += "**Executive Summary:** " + r.executive_summary + "\n\n";
            summary += "**Risk Assessment:** " + r.risk_assessment;
            let summary_block = #{
                type: "mdsvex",
                content: summary
            };

            // Key Findings table
            let finding_rows = [];
            let i = 1;
            for f in r.key_findings {
                finding_rows.push([i.to_string(), f]);
                i += 1;
            }
            let findings_block = #{
                type: "table",
                headers: ["#", "Finding"],
                rows: finding_rows,
                caption: "Key Findings"
            };

            // Recommendations table
            let rec_rows = [];
            let j = 1;
            for rec in r.recommendations {
                rec_rows.push([j.to_string(), rec]);
                j += 1;
            }
            let recs_block = #{
                type: "table",
                headers: ["#", "Recommendation"],
                rows: rec_rows,
                caption: "Recommendations"
            };

            // Confidence callout
            let score_block = #{
                type: "callout",
                severity: "info",
                title: "Confidence Score",
                content: "**" + r.confidence_score.to_string() + " / 1.0**"
            };

            // Raw data download (served from RustFS S3)
            let download_block = #{
                type: "download",
                downloads: [#{
                    url: "http://localhost:9005/research/raw_data.json",
                    filename: "raw_data.json",
                    mime_type: "application/json",
                    size: data.bytes_transferred,
                    description: "Raw research data stored in RustFS S3"
                }]
            };

            #{
                form: #{
                    title: "Research Brief Review: " + data.topic,
                    instructions_mdsvex: "Review the AI-generated research brief below. Approve if the analysis is satisfactory, or reject with feedback.",
                    steps: [
                        #{
                            id: "review",
                            title: "Review Report",
                            description_mdsvex: "**Topic:** " + data.topic,
                            blocks: [
                                summary_block,
                                #{ type: "divider" },
                                findings_block,
                                recs_block,
                                #{ type: "divider" },
                                score_block,
                                download_block,
                                #{ type: "divider" },
                                #{
                                    type: "input",
                                    field: #{
                                        name: "decision",
                                        label: "Decision",
                                        kind: "select",
                                        options: ["approve", "reject"],
                                        required: true
                                    }
                                },
                                #{
                                    type: "input",
                                    field: #{
                                        name: "comments",
                                        label: "Review Comments",
                                        kind: "textarea",
                                        required: false
                                    }
                                }
                            ]
                        }
                    ],
                    topic: data.topic,
                    report: data.report
                }
            }
        "##,
        );

    // 13. request_review — human_task effect creates the review in the UI
    ctx.transition("request_review", "Request Report Review")
        .process_step_started("review")
        .read_input("process", &processes)
        .human_task_to(HumanTaskSubmit {
            task: &review_form,
            assigned: &review_task,
            errors: &effect_errors,
            response_signal: &sig_review_response,
        });

    // 14. approve_review — human approved the report
    ctx.transition("approve_review", "Approve Report")
        .process_step_completed("review")
        .read_input("process", &processes)
        .auto_input("task", &review_task)
        .auto_input("response", &sig_review_response)
        .guard(r#"response.task_id == task.task_id && response.data.decision == "approve""#)
        .auto_output("out", &approved)
        .auto_output("done", &process_done)
        .logic(
            r#"#{
                out: #{
                    topic: task.topic,
                    report: task.report,
                    reviewer_comments: if response.data.comments == () { "" } else { response.data.comments }
                },
                done: #{}
            }"#,
        );

    // 15. reject_review — human rejected the report
    ctx.transition("reject_review", "Reject Report")
        .process_step_completed("review")
        .read_input("process", &processes)
        .auto_input("task", &review_task)
        .auto_input("response", &sig_review_response)
        .guard(r#"response.task_id == task.task_id && response.data.decision == "reject""#)
        .auto_output("out", &rejected)
        .auto_output("done", &process_done)
        .logic(
            r#"#{
                out: #{
                    topic: task.topic,
                    reason: if response.data.comments == () { "Rejected without comment" } else { response.data.comments }
                },
                done: #{}
            }"#,
        );

    // -------------------------------------------------------------------------
    // Failure paths
    // -------------------------------------------------------------------------

    // 16. fail_python
    ctx.transition("fail_python", "Fail Python Step")
        .auto_input("fail", &failure_inbox)
        .auto_input("pending", &python_pending)
        .correlate("fail", "pending", "job_id")
        .auto_output("out", &workflow_failed)
        .logic(
            r#"#{
                out: #{
                    phase: pending.phase,
                    job_id: pending.job_id,
                    reason: fail.reason
                }
            }"#,
        );

    // 17. fail_file
    ctx.transition("fail_file", "Fail File Transfer Step")
        .auto_input("fail", &failure_inbox)
        .auto_input("pending", &file_pending)
        .correlate("fail", "pending", "job_id")
        .auto_output("out", &workflow_failed)
        .logic(
            r#"#{
                out: #{
                    phase: pending.phase,
                    job_id: pending.job_id,
                    reason: fail.reason
                }
            }"#,
        );

    // 18. fail_llm
    ctx.transition("fail_llm", "Fail LLM Step")
        .auto_input("fail", &failure_inbox)
        .auto_input("pending", &llm_pending)
        .correlate("fail", "pending", "job_id")
        .auto_output("out", &workflow_failed)
        .logic(
            r#"#{
                out: #{
                    phase: pending.phase,
                    job_id: pending.job_id,
                    reason: fail.reason
                }
            }"#,
        );
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "research-brief-orchestrator",
        "Research Brief Generator: orchestration net with human tasks, Python, file-ops, and LLM",
        definition,
    );
}
