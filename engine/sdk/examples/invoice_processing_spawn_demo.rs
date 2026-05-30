//! Layer 0: Invoice Processing Orchestrator — full-feature demo with dynamic child net spawning.
//!
//! Demonstrates 10+ capabilities in a single coherent workflow:
//! 1. **Human UI (data entry)** — AP clerk uploads invoice document, enters metadata
//! 2. **Vision LLM (OCR)** — extracts structured data from invoice images (GLM-OCR)
//! 3. **Kreuzberg (document extraction)** — extracts text from PDFs, Office docs, etc.
//! 4. **Text LLM (structured parsing)** — parses kreuzberg text into structured JSON
//! 5. **Python executor** — validates totals, flags anomalies, vendor check
//! 6. **Vault secret injection** — VALIDATION_API_KEY wrapped for executor
//! 7. **Multi-storage I/O** — per-input S3 download, per-output S3 upload
//! 8. **Text LLM (summary)** — generates approval recommendation with risk assessment
//! 9. **Human UI (review)** — all block types: image, mdsvex, table, callout, download, divider, input, signature
//! 10. **Timer (SLA escalation)** — 5-minute deadline for review
//! 11. **Dynamic child net spawning** — each execution step spawns its own child net
//!
//! ## Architecture: Dynamic Spawn (replaces static 4-layer bridges)
//!
//! Each execution step (OCR, Kreuzberg, Parse, Validation, Summary) calls `ctx.spawn()`
//! to create a dedicated child net that encapsulates the full executor lifecycle:
//!
//! ```text
//! orchestrator-net ──spawn──► child-net (per step)
//!                              │ inbox → submit → signals → retry/complete
//!                   ◄─bridge── │ reply_out ($params.parent_net_id)
//! ```
//!
//! ## Data flow (control-token + data-token pattern)
//!
//! Control tokens drive the pipeline; data tokens are parked and read via read arcs.
//!
//! ## Deploy
//!
//! ```bash
//! just demo invoice-processing
//! ```
//!
//! ## Net ID: `orchestrator-net`

mod common;

use aithericon_sdk::prelude::*;
use common::executor_lifecycle::{executor_lifecycle, ExecutorBridges};

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Parsed invoice parameters from human entry.
#[token]
struct InvoiceParams {
    department: String,
    urgency: String,
    notes: String,
    document_key: String,
    document_type: String,
}

/// Structured invoice data extracted — control signal (data lives in extracted_data place).
#[token]
struct DataExtracted {}

/// Parked data: extracted invoice data (written by join_ocr or join_parse).
#[token]
struct ExtractedInvoice {
    invoice_data: serde_json::Value,
}

/// Kreuzberg raw text extraction completed (control token — carries extraction output for parse).
#[token]
struct ExtractDone {
    content: String,
    tables: serde_json::Value,
}

/// Python validation completed — control signal (data lives in validation_result place).
#[token]
struct ValidationDone {}

/// Parked data: validation results (written by join_validation).
#[token]
struct ValidationResult {
    validated_data: serde_json::Value,
    risk_flags: serde_json::Value,
    compliance_status: String,
    anomalies: serde_json::Value,
    validation_score: f64,
}

/// LLM summary completed — carries only summary output (validation data in validation_result place).
#[token]
struct SummaryDone {
    recommendation: String,
    risk_level: String,
    summary: String,
    key_concerns: serde_json::Value,
    suggested_action: String,
}

/// Review form — extends HumanTaskRequest with `invoice_number` for downstream correlation.
#[token]
struct ReviewForm {
    title: String,
    instructions_mdsvex: String,
    steps: Vec<TaskStep>,
    invoice_number: String,
}

/// Assigned review task.
#[token]
struct ReviewTask {
    task_id: String,
    invoice_number: String,
}

/// Approved invoice.
#[token]
struct ApprovedInvoice {
    invoice_number: String,
    approved_by: String,
    comments: String,
}

/// Rejected invoice.
#[token]
struct RejectedInvoice {
    invoice_number: String,
    rejected_by: String,
    reason: String,
}

/// Escalated invoice (SLA breach).
#[token]
struct EscalatedInvoice {
    invoice_number: String,
    original_reviewer: String,
    escalation_reason: String,
}

/// Terminal workflow failure.
#[token]
struct WorkflowFailed {
    phase: String,
    job_id: String,
    reason: String,
}

// ---------------------------------------------------------------------------
// Child net: executor lifecycle (uses shared module)
// ---------------------------------------------------------------------------

/// Build a child executor lifecycle net.
///
/// Receives a job spec via `io.inbox`, submits to the executor, tracks the full
/// lifecycle via signals (accepted → running → completed/failed/timed_out),
/// retries on failure and timeout, handles cancellation and mid-execution
/// events, and bridges the result/failure back to the parent.
///
/// Reply uses `BridgeReply` (auto-correlated via `ReplyRouting`).
/// Failure uses `bridge_out_param` (via `$params.parent_net_id`).
fn executor_child_net(child: &mut Context, io: SpawnChildIO) {
    executor_lifecycle(
        child,
        ExecutorBridges {
            inbox: io.inbox.retyped(),
            result_out: Some(io.reply),
            failure_out: Some(io.failure),
            process_id: None,
            process_step: None,
            catalogue: true,
            process: false,
            stream_log: None,
        },
    );
}

// ---------------------------------------------------------------------------
// Net definition
// ---------------------------------------------------------------------------

fn definition(ctx: &mut Context) {
    // Cross-cutting terminals / errors (written by transitions in multiple scopes)
    let workflow_failed = ctx.state::<WorkflowFailed>("workflow_failed", "Workflow Failed");
    let effect_errors = ctx.state::<EffectError>("effect_errors", "Effect Errors");

    // Embedded scripts
    let validation_script =
        include_str!("../../demos/invoice_processing/validate_invoice.py");

    // ── Shared Rhai constants ─────────────────────────────────────────────

    ctx.rhai_const("S3_STORAGE", r#"#{
        backend: "s3",
        endpoint: "http://localhost:9005",
        bucket: "human-tasks",
        region: "us-east-1",
        credentials: #{ access_key: "{{secret:demo/rustfs#access_key}}", secret_key: "{{secret:demo/rustfs#secret_key}}" }
    }"#);

    ctx.rhai_const("INVOICE_SCHEMA", r#"#{
        type: "object",
        properties: #{
            vendor: #{ type: "string" },
            invoice_number: #{ type: "string" },
            date: #{ type: "string" },
            line_items: #{ type: "array", items: #{
                type: "object",
                properties: #{ description: #{ type: "string" }, quantity: #{ type: "number" }, unit_price: #{ type: "number" }, amount: #{ type: "number" } },
                required: ["description", "quantity", "unit_price", "amount"]
            }},
            subtotal: #{ type: "number" },
            tax: #{ type: "number" },
            total: #{ type: "number" },
            payment_terms: #{ type: "string" }
        },
        required: ["vendor", "invoice_number", "date", "line_items", "total"]
    }"#);

    ctx.rhai_var("VALIDATION_SCRIPT_JSON", validation_script);

    // ─── Process Lifecycle ───────────────────────────────────────────────

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
                config: ProcessStartConfig::new("Invoice Processing")
                    .process_id_prefix("inv-")
                    .human_step("entry", "Data Entry")
                    .step("extraction", "Data Extraction")
                    .step("validation", "Cross-Validation")
                    .step("summary", "Summary Generation")
                    .human_step("review", "Manager Review"),
            });

        ctx.transition("complete_process", "Complete Process")
            .process_complete_to(ProcessComplete {
                process: &processes,
                done: &process_done,
                completed: &process_completed,
            });

        (process_done, processes)
    });

    // ─── 1. Data Entry ───────────────────────────────────────────────────

    let (invoice_params, entry_data) = ctx.scope("1. Data Entry", |ctx| {
        let invoice_params = ctx.state::<InvoiceParams>("invoice_params", "Invoice Parameters");
        let entry_data = ctx.state::<InvoiceParams>("entry_data", "Original Entry Data");
        let entry_form = ctx.state::<HumanTaskRequest>("entry_form", "Entry Form");
        let entry_task = ctx.state::<HumanTaskAssigned>("entry_task", "Entry Task");
        let sig_entry_response =
            ctx.signal::<HumanTaskResponse>("sig_entry_response", "Entry Response Signal");

        ctx.seed(
            &entry_form,
            vec![HumanTaskRequest {
                task_id: None,
                net_id: None,
                org_id: None,
                place: None,
                corr_id: None,
                title: "Invoice Processing Request".into(),
                instructions_mdsvex: Some("Upload an invoice document and provide processing metadata. \
                    The system will extract the data (Vision OCR for images, Kreuzberg for PDFs/Office docs), \
                    validate it, archive to S3, generate an approval recommendation, and route it for review.\n\n\
                    A sample invoice is available at `http://localhost:8080/sample_invoice.png`."
                    .into()),
                payload: None,
                response_subject: None,
                process_id: None,
                process_step: None,
                steps: vec![TaskStep {
                    id: "invoice".into(),
                    title: "Invoice Details".into(),
                    description_mdsvex: Some("Upload the invoice document for processing.".into()),
                    blocks: vec![
                        TaskBlock::Input {
                            field: TaskField::file("invoice_file", "Invoice Document")
                                .required()
                                .description("Upload an invoice (PNG, JPG, PDF, Word, Excel, or other document). Images use Vision OCR; documents use Kreuzberg text extraction. A sample is available at [localhost:8080/sample_invoice.png](http://localhost:8080/sample_invoice.png).")
                                .accept("image/*,.pdf,.doc,.docx,.xls,.xlsx,.ppt,.pptx,.odt,.rtf,.txt,.csv")
                                .max_files(1),
                        },
                        TaskBlock::Divider,
                        TaskBlock::Input {
                            field: TaskField::select("department", "Department")
                                .required()
                                .options(&["engineering", "marketing", "operations", "finance", "hr"]),
                        },
                        TaskBlock::Input {
                            field: TaskField::select("urgency", "Processing Urgency")
                                .required()
                                .options(&["standard", "rush", "critical"]),
                        },
                        TaskBlock::Input {
                            field: TaskField::textarea("notes", "Additional Notes")
                                .placeholder("Any context for the reviewer (PO number, project code, etc.)"),
                        },
                    ],
                }],
            }],
        );

        ctx.transition("request_entry", "Request Invoice Entry")
            .process_step_started("entry")
            .read_input("process", &processes)
            .human_task_to(HumanTaskSubmit {
                task: &entry_form,
                assigned: &entry_task,
                errors: &effect_errors,
                response_signal: &sig_entry_response,
            });

        ctx.transition("parse_entry", "Parse Entry Response")
            .process_step_completed("entry")
            .read_input("process", &processes)
            .auto_input("task", &entry_task)
            .auto_input("response", &sig_entry_response)
            .guard(r#"response.task_id == task.task_id"#)
            .auto_output("params", &invoice_params)
            .auto_output("artifact", &entry_data)
            .logic(
                r#"
                let files = response.data.invoice_file;
                let doc_key = files[0].key;
                let doc_type = if files[0].type != () { files[0].type } else { "application/octet-stream" };
                let dept = response.data.department;
                let urg = response.data.urgency;
                let notes_val = if response.data.notes == () { "" } else { response.data.notes };
                #{
                    params: #{
                        department: dept,
                        urgency: urg,
                        notes: notes_val,
                        document_key: doc_key,
                        document_type: doc_type
                    },
                    artifact: #{
                        department: dept,
                        urgency: urg,
                        notes: notes_val,
                        document_key: doc_key,
                        document_type: doc_type
                    }
                }
            "#,
            );

        (invoice_params, entry_data)
    });

    // ─── 2. Data Extraction ──────────────────────────────────────────────

    let (data_extracted, extracted_data) = ctx.scope("2. Data Extraction", |ctx| {
        let data_extracted = ctx.state::<DataExtracted>("data_extracted", "Data Extracted");
        let extracted_data =
            ctx.state::<ExtractedInvoice>("extracted_data", "Extracted Invoice Data");

        // ── OCR (Images) ─────────────────────────────────────────────
        ctx.scope("OCR (Images)", |ctx| {
            let ocr = ctx.spawn::<DynamicToken>("ocr", executor_child_net);

            ocr.prepare(ctx, "Prepare OCR Job (Image)")
                .process_step_started("extraction")
                .read_input("process", &processes)
                .read_input("entry", &entry_data)
                .auto_input("params", &invoice_params)
                .guard(r#"params.document_type.starts_with("image")"#)
                .spawn_logic_labeled(
                    r#"
                    let job_id = "invoice:ocr:" + entry.department;
                    let config = #{
                        provider: "ollama",
                        model: "glm-ocr:q8_0",
                        base_url: "http://localhost:11434",
                        prompt: "You are an expert invoice OCR system. Analyze the invoice image and extract ALL structured data. Return a JSON object with these exact fields: vendor (company name), invoice_number (string), date (YYYY-MM-DD), line_items (array of objects with description, quantity, unit_price, amount), subtotal (number), tax (number), total (number), payment_terms (string). Be precise with numbers.",
                        images: [#{ path: "{{input_path:invoice_document}}" }],
                        response_format: #{ type: "json_schema", schema: INVOICE_SCHEMA }
                    };
                    let spec = #{
                        type: "llm",
                        config: config,
                        inputs: [#{
                            name: "invoice_document",
                            source: #{ type: "storage_path", path: params.document_key, storage: S3_STORAGE },
                            required: true
                        }],
                        outputs: [#{ name: "response", required: true }]
                    };
                    #{
                        job_id: job_id,
                        run: 0,
                        retries: 0,
                        max_retries: 2,
                        spec: spec
                    }
                "#,
                    r#""OCR: " + entry.department"#,
                );

            ocr.join(ctx, "Join OCR Result")
                .process_step_completed("extraction")
                .read_input("process", &processes)
                .auto_output("invoice", &extracted_data)
                .auto_output("ctrl", &data_extracted)
                .logic(
                    r#"#{
                        invoice: #{
                            invoice_data: result.detail.outputs.response
                        },
                        ctrl: #{}
                    }"#,
                );

            ocr.on_failure(ctx, &workflow_failed, "ocr");
        });

        // ── Kreuzberg (Documents) ────────────────────────────────────
        let extract_done = ctx.scope("Kreuzberg (Documents)", |ctx| {
            let extract = ctx.spawn::<DynamicToken>("extract", executor_child_net);
            let extract_done = ctx.state::<ExtractDone>("extract_done", "Extract Done");

            extract.prepare(ctx, "Prepare Extract Job (Document)")
                .process_step_started("extraction")
                .read_input("process", &processes)
                .read_input("entry", &entry_data)
                .auto_input("params", &invoice_params)
                .guard(r#"!params.document_type.starts_with("image")"#)
                .spawn_logic_labeled(
                    r#"
                    let job_id = "invoice:extract:" + entry.department;
                    let spec = #{
                        type: "kreuzberg",
                        config: #{
                            mode: "single",
                            file: "document",
                            mime_type: params.document_type
                        },
                        inputs: [#{
                            name: "document",
                            source: #{ type: "storage_path", path: params.document_key, storage: S3_STORAGE },
                            required: true
                        }],
                        outputs: [
                            #{ name: "content", required: true },
                            #{ name: "tables", required: false }
                        ]
                    };
                    #{
                        job_id: job_id,
                        run: 0,
                        retries: 0,
                        max_retries: 2,
                        spec: spec
                    }
                "#,
                    r#""Extract: " + entry.department"#,
                );

            extract.join(ctx, "Join Extract Result")
                .auto_output("done", &extract_done)
                .logic(
                    r#"
                    let tables = if result.detail.outputs.tables != () { result.detail.outputs.tables } else { [] };
                    #{
                        done: #{
                            content: result.detail.outputs.content,
                            tables: tables
                        }
                    }
                "#,
                );

            extract.on_failure(ctx, &workflow_failed, "extract");

            extract_done
        });

        // ── Text Parse ───────────────────────────────────────────────
        ctx.scope("Text Parse", |ctx| {
            let parse = ctx.spawn::<DynamicToken>("parse", executor_child_net);

            parse.prepare(ctx, "Prepare Parse Job")
                .read_input("entry", &entry_data)
                .auto_input("data", &extract_done)
                .spawn_logic_labeled(
                    r#"
                    let job_id = "invoice:parse:" + entry.department;

                    let table_text = "";
                    if data.tables != () && data.tables.len() > 0 {
                        table_text = "\n\nDetected tables:\n";
                        for tbl in data.tables {
                            if tbl.markdown != () {
                                table_text += tbl.markdown + "\n";
                            }
                        }
                    }

                    let config = #{
                        provider: "ollama",
                        model: "gpt-oss:20b",
                        base_url: "http://localhost:11434",
                        prompt: "You are an expert invoice data extraction system. Parse the following document text into structured invoice JSON. Extract ALL fields precisely. Return a JSON object with: vendor (company name), invoice_number (string), date (YYYY-MM-DD), line_items (array of objects with description, quantity, unit_price, amount), subtotal (number), tax (number), total (number), payment_terms (string).\n\nDocument text:\n" + data.content + table_text,
                        response_format: #{ type: "json_schema", schema: INVOICE_SCHEMA }
                    };
                    let spec = #{
                        type: "llm",
                        config: config,
                        inputs: [],
                        outputs: [#{ name: "response", required: true }]
                    };
                    #{
                        job_id: job_id,
                        run: 0,
                        retries: 0,
                        max_retries: 2,
                        spec: spec
                    }
                "#,
                    r#""Parse: " + entry.department"#,
                );

            parse.join(ctx, "Join Parse Result")
                .process_step_completed("extraction")
                .read_input("process", &processes)
                .auto_output("invoice", &extracted_data)
                .auto_output("ctrl", &data_extracted)
                .logic(
                    r#"#{
                        invoice: #{
                            invoice_data: result.detail.outputs.response
                        },
                        ctrl: #{}
                    }"#,
                );

            parse.on_failure(ctx, &workflow_failed, "parse");
        });

        (data_extracted, extracted_data)
    });

    // ─── 3. Validation ───────────────────────────────────────────────────

    let (validation_done, validation_result) = ctx.scope("3. Validation", |ctx| {
        let validation_done = ctx.state::<ValidationDone>("validation_done", "Validation Done");
        let validation_result =
            ctx.state::<ValidationResult>("validation_result", "Validation Result");
        let validation = ctx.spawn::<DynamicToken>("validation", executor_child_net);

        validation.prepare(ctx, "Prepare Validation Job")
            .process_step_started("validation")
            .read_input("process", &processes)
            .read_input("invoice", &extracted_data)
            .read_input("entry", &entry_data)
            .auto_input("ctrl", &data_extracted)
            .spawn_logic_labeled(
                r#"
                let invoice_num = if invoice.invoice_data.invoice_number != () {
                    invoice.invoice_data.invoice_number
                } else {
                    "unknown"
                };
                let job_id = "invoice:validate:" + invoice_num;
                let spec = #{
                    type: "python",
                    config: #{
                        script: "validate_invoice.py",
                        virtualenv: true,
                        sdk: true,
                        env: #{
                            VALIDATION_API_KEY: "{{secret:demo/validation#api_key}}"
                        }
                    },
                    inputs: [
                        #{
                            name: "validate_invoice.py",
                            source: #{
                                type: "raw",
                                content: VALIDATION_SCRIPT_JSON
                            },
                            required: true
                        },
                        #{
                            name: "invoice_data.json",
                            source: #{
                                type: "inline",
                                value: #{
                                    vendor: invoice.invoice_data.vendor,
                                    invoice_number: invoice_num,
                                    date: invoice.invoice_data.date,
                                    line_items: invoice.invoice_data.line_items,
                                    subtotal: invoice.invoice_data.subtotal,
                                    tax: invoice.invoice_data.tax,
                                    total: invoice.invoice_data.total,
                                    payment_terms: invoice.invoice_data.payment_terms,
                                    department: entry.department,
                                    urgency: entry.urgency
                                }
                            },
                            required: true
                        }
                    ],
                    outputs: [
                        #{ name: "result", required: true },
                        #{ name: "validated_invoice.json", path: "validated_invoice.json", required: true,
                            upload_to: #{
                                storage: #{
                                    backend: "s3",
                                    endpoint: "http://localhost:9005",
                                    bucket: "invoices",
                                    region: "us-east-1",
                                    credentials: #{ access_key: "{{secret:demo/rustfs#access_key}}", secret_key: "{{secret:demo/rustfs#secret_key}}" }
                                },
                                destination_path: "validated_invoice.json"
                            }
                        }
                    ]
                };
                #{
                    job_id: job_id,
                    run: 0,
                    retries: 0,
                    max_retries: 2,
                    spec: spec
                }
            "#,
                r#"if invoice.invoice_data.invoice_number != () { "Validate: " + invoice.invoice_data.invoice_number } else { "Validate" }"#,
            );

        validation.join(ctx, "Join Validation Result")
            .process_step_completed("validation")
            .read_input("process", &processes)
            .auto_output("vresult", &validation_result)
            .auto_output("ctrl", &validation_done)
            .logic(
                r#"
                let v = result.detail.outputs.result;
                #{
                    vresult: #{
                        validated_data: v.validated_data,
                        risk_flags: v.risk_flags,
                        compliance_status: v.compliance_status,
                        anomalies: v.anomalies,
                        validation_score: v.validation_score
                    },
                    ctrl: #{}
                }
            "#,
            );

        validation.on_failure(ctx, &workflow_failed, "validation");

        (validation_done, validation_result)
    });

    // ─── 4. Summary ──────────────────────────────────────────────────────

    let summary_done = ctx.scope("4. Summary", |ctx| {
        let summary_done = ctx.state::<SummaryDone>("summary_done", "Summary Done");
        let summary = ctx.spawn::<DynamicToken>("summary", executor_child_net);

        summary.prepare(ctx, "Prepare Summary Job")
            .process_step_started("summary")
            .read_input("process", &processes)
            .read_input("vresult", &validation_result)
            .auto_input("ctrl", &validation_done)
            .spawn_logic_labeled(
                r#"
                let invoice_num = if vresult.validated_data.invoice_number != () {
                    vresult.validated_data.invoice_number
                } else {
                    "unknown"
                };
                let job_id = "invoice:summary:" + invoice_num;

                let risk_text = "";
                for flag in vresult.risk_flags {
                    risk_text += "- " + flag + "\n";
                }
                if risk_text == "" {
                    risk_text = "No risk flags identified.";
                }

                let spec = #{
                    type: "llm",
                    config: #{
                        provider: "ollama",
                        model: "gpt-oss:20b",
                        base_url: "http://localhost:11434",
                        prompt: "You are a financial compliance analyst. Review the following invoice validation results and generate an approval recommendation.\n\nInvoice: " + invoice_num + "\nVendor: " + vresult.validated_data.vendor + "\nTotal: $" + vresult.validated_data.total.to_string() + "\nCompliance Status: " + vresult.compliance_status + "\nValidation Score: " + vresult.validation_score.to_string() + "\n\nRisk Flags:\n" + risk_text + "\n\nProvide a structured recommendation.",
                        response_format: #{
                            type: "json_schema",
                            schema: #{
                                type: "object",
                                properties: #{
                                    recommendation: #{ type: "string" },
                                    risk_level: #{ type: "string" },
                                    summary: #{ type: "string" },
                                    key_concerns: #{ type: "array", items: #{ type: "string" } },
                                    suggested_action: #{ type: "string" }
                                },
                                required: ["recommendation", "risk_level", "summary", "suggested_action"]
                            }
                        }
                    },
                    inputs: [
                        #{
                            name: "validation_data.json",
                            source: #{
                                type: "inline",
                                value: #{
                                    validated_data: vresult.validated_data,
                                    risk_flags: vresult.risk_flags,
                                    compliance_status: vresult.compliance_status,
                                    validation_score: vresult.validation_score
                                }
                            },
                            required: true
                        }
                    ],
                    outputs: [#{ name: "response", required: true }]
                };
                #{
                    job_id: job_id,
                    run: 0,
                    retries: 0,
                    max_retries: 1,
                    spec: spec
                }
            "#,
                r#"if vresult.validated_data.invoice_number != () { "Summary: " + vresult.validated_data.invoice_number } else { "Summary" }"#,
            );

        summary.join(ctx, "Join Summary Result")
            .process_step_completed("summary")
            .read_input("process", &processes)
            .auto_output("done", &summary_done)
            .logic(
                r#"
                let r = result.detail.outputs.response;
                #{
                    done: #{
                        recommendation: r.recommendation,
                        risk_level: if r.risk_level != () { r.risk_level } else { "unknown" },
                        summary: r.summary,
                        key_concerns: if r.key_concerns != () { r.key_concerns } else { [] },
                        suggested_action: r.suggested_action
                    }
                }
            "#,
            );

        summary.on_failure(ctx, &workflow_failed, "summary");

        summary_done
    });

    // ─── 5. Review & SLA ─────────────────────────────────────────────────

    ctx.scope("5. Review & SLA", |ctx| {
        let approved = ctx.state::<ApprovedInvoice>("approved", "Approved Invoices");
        let rejected = ctx.state::<RejectedInvoice>("rejected", "Rejected Invoices");
        let escalated = ctx.state::<EscalatedInvoice>("escalated", "Escalated Invoices");
        let review_form = ctx.state::<HumanTaskRequest>("review_form", "Review Form");
        let review_task = ctx.state::<HumanTaskAssigned>("review_task", "Review Task");
        let sig_review_response =
            ctx.signal::<HumanTaskResponse>("sig_review_response", "Review Response Signal");
        let review_tracking = ctx.state::<DynamicToken>("sla_tracking", "SLA Tracking");
        let task_to_cancel = ctx.state::<HumanCancelInput>("task_to_cancel", "Tasks to Cancel");

        ctx.transition("prepare_review", "Prepare Review Form")
            .read_input("vresult", &validation_result)
            .read_input("entry", &entry_data)
            .auto_input("data", &summary_done)
            .auto_output("form", &review_form)
            .logic(
                r###"
                let d = vresult.validated_data;
                let invoice_num = if d.invoice_number != () { d.invoice_number } else { "N/A" };
                let vendor = if d.vendor != () { d.vendor } else { "Unknown" };
                let total_str = if d.total != () { d.total.to_string() } else { "0" };

                // 1. Document block — original uploaded document from S3
                let doc_url = "http://localhost:9005/human-tasks/" + entry.document_key;
                let doc_block = if entry.document_type.starts_with("image") {
                    #{ type: "image", url: doc_url, alt: "Original Invoice", caption: "Invoice " + invoice_num + " from " + vendor }
                } else {
                    let parts = entry.document_key.split("/");
                    let filename = parts[parts.len() - 1];
                    #{ type: "download", downloads: [#{ url: doc_url, filename: filename, mime_type: entry.document_type, description: "Original uploaded document" }] }
                };

                // 2. Mdsvex block — AI recommendation summary (from summary_done)
                let summary_text = "## AI Recommendation\n\n";
                summary_text += "**" + data.recommendation + "**\n\n";
                summary_text += data.summary + "\n\n";
                summary_text += "**Suggested Action:** " + data.suggested_action;
                let summary_block = #{
                    type: "mdsvex",
                    content: summary_text
                };

                // 3. Table block — extracted line items (from validation_result)
                let item_rows = [];
                let idx = 1;
                if d.line_items != () {
                    for item in d.line_items {
                        let desc = if item.description != () { item.description } else { "?" };
                        let qty = if item.quantity != () { item.quantity.to_string() } else { "?" };
                        let price = if item.unit_price != () { "$" + item.unit_price.to_string() } else { "?" };
                        let amt = if item.amount != () { "$" + item.amount.to_string() } else { "?" };
                        item_rows.push([idx.to_string(), desc, qty, price, amt]);
                        idx += 1;
                    }
                }
                let table_block = #{
                    type: "table",
                    headers: ["#", "Description", "Qty", "Unit Price", "Amount"],
                    rows: item_rows,
                    caption: "Extracted Line Items (Subtotal: $" + d.subtotal.to_string() + " | Tax: $" + d.tax.to_string() + " | Total: $" + total_str + ")"
                };

                // 4. Callout block — risk assessment (from validation_result + summary_done)
                let risk_severity = if data.risk_level == "high" { "error" } else if data.risk_level == "medium" { "warning" } else { "info" };
                let concerns_text = "";
                if data.key_concerns != () {
                    for concern in data.key_concerns {
                        concerns_text += "- " + concern + "\n";
                    }
                }
                if concerns_text == "" {
                    concerns_text = "No significant concerns identified.";
                }
                concerns_text += "\n**Validation Score:** " + vresult.validation_score.to_string() + " / 1.0";
                concerns_text += "\n**Compliance:** " + vresult.compliance_status;

                let risk_flags_text = "";
                if vresult.risk_flags != () {
                    for flag in vresult.risk_flags {
                        risk_flags_text += "- " + flag + "\n";
                    }
                }
                if risk_flags_text != "" {
                    concerns_text += "\n\n**Risk Flags:**\n" + risk_flags_text;
                }

                let callout_block = #{
                    type: "callout",
                    severity: risk_severity,
                    title: "Risk Assessment: " + data.risk_level,
                    content: concerns_text
                };

                // 5. Download block — validated data uploaded to S3 via per-output upload_to
                let download_block = #{
                    type: "download",
                    downloads: [#{
                        url: "http://localhost:9005/invoices/validated_invoice.json",
                        filename: "validated_invoice.json",
                        mime_type: "application/json",
                        description: "Validated invoice data archived in RustFS S3"
                    }]
                };

                // Assemble the form with all 7 block types
                #{
                    form: #{
                        title: "Invoice Review: " + invoice_num,
                        instructions_mdsvex: "Review the AI-processed invoice below. Approve, reject, or escalate. You have **5 minutes** before this is automatically escalated.",
                        steps: [
                            #{
                                id: "review",
                                title: "Review Invoice",
                                description_mdsvex: "**Vendor:** " + vendor + " | **Total:** $" + total_str + " | **Department:** " + entry.department,
                                blocks: [
                                    doc_block,
                                    #{ type: "divider" },
                                    summary_block,
                                    #{ type: "divider" },
                                    table_block,
                                    #{ type: "divider" },
                                    callout_block,
                                    download_block,
                                    #{ type: "divider" },
                                    #{
                                        type: "input",
                                        field: #{
                                            name: "decision",
                                            label: "Decision",
                                            kind: "select",
                                            options: ["approve", "reject", "escalate"],
                                            required: true
                                        }
                                    },
                                    #{
                                        type: "input",
                                        field: #{
                                            name: "comments",
                                            label: "Review Comments",
                                            kind: "textarea",
                                            required: false,
                                            placeholder: "Justification for your decision..."
                                        }
                                    },
                                    #{
                                        type: "input",
                                        field: #{
                                            name: "signature",
                                            label: "Reviewer Signature",
                                            kind: "signature",
                                            required: true,
                                            description_mdsvex: "Sign to confirm your review decision."
                                        }
                                    }
                                ]
                            }
                        ],
                        invoice_number: invoice_num
                    }
                }
            "###,
            );

        ctx.transition("request_review", "Request Invoice Review")
            .process_step_started("review")
            .read_input("process", &processes)
            .human_task_to(HumanTaskSubmit {
                task: &review_form,
                assigned: &review_task,
                errors: &effect_errors,
                response_signal: &sig_review_response,
            });

        // ── SLA Timer ────────────────────────────────────────────────
        let (timer_scheduled, timer_to_cancel, sig_sla_timeout) =
            ctx.scope("SLA Timer", |ctx| {
                let timer_data = ctx.state::<TimerInput>("timer_data", "Timer Data");
                let timer_scheduled = ctx.state::<TimerScheduled>("timer_scheduled", "Timer Scheduled");
                let timer_to_cancel = ctx.state::<TimerCancelInput>("timer_to_cancel", "Timers to Cancel");
                let timer_cancelled = ctx.state::<TimerCancelled>("timer_cancelled", "Cancelled Timers");
                let sig_sla_timeout = ctx.signal::<DynamicToken>("sig_sla_timeout", "SLA Timeout Signal");

                ctx.transition("start_review_timer", "Start Review Timer")
                    .auto_input("task", &review_task)
                    .auto_output("tracking", &review_tracking)
                    .auto_output("timer", &timer_data)
                    .logic(format!(
                        r#"
                        {{
                            let tracking = #{{
                                invoice_number: task.invoice_number,
                                task_id: task.task_id,
                                reviewer: "finance-reviewer",
                                timeout_minutes: 5
                            }};
                            let timer = #{{
                                delay_ms: 300000,
                                target_place_id: "{}",
                                payload: #{{
                                    invoice_number: task.invoice_number,
                                    task_id: task.task_id
                                }}
                            }};
                            #{{ tracking: tracking, timer: timer }}
                        }}
                    "#,
                        sig_sla_timeout.id()
                    ));

                ctx.transition("schedule_review_timer", "Schedule Review Timer")
                    .timer_schedule_to(TimerSchedule {
                        timer: &timer_data,
                        scheduled: &timer_scheduled,
                        errors: &effect_errors,
                        signal: &sig_sla_timeout,
                    });

                ctx.transition("cancel_review_timer", "Cancel Review Timer")
                    .timer_cancel_to(TimerCancel {
                        timer: &timer_to_cancel,
                        cancelled: &timer_cancelled,
                        errors: &effect_errors,
                    });

                (timer_scheduled, timer_to_cancel, sig_sla_timeout)
            });

        // ── Resolution ───────────────────────────────────────────────
        ctx.scope("Resolution", |ctx| {
            ctx.transition("human_approved", "Human Approved")
                .process_step_completed("review")
                .read_input("process", &processes)
                .auto_input("tracking", &review_tracking)
                .auto_input("timer", &timer_scheduled)
                .auto_input("response", &sig_review_response)
                .guard(
                    r#"response.task_id == tracking.task_id && timer.payload.task_id == tracking.task_id && response.data.decision == "approve""#,
                )
                .auto_output("approved", &approved)
                .auto_output("cancel", &timer_to_cancel)
                .auto_output("process", &process_done)
                .logic(
                    r#"
                    #{
                        approved: #{
                            invoice_number: tracking.invoice_number,
                            approved_by: tracking.reviewer,
                            comments: if response.data.comments == () { "" } else { response.data.comments }
                        },
                        cancel: #{
                            timer_correlation_id: timer.timer_correlation_id,
                            target_place_id: timer.target_place_id
                        },
                        process: #{
                            outcome: "approved",
                            invoice_number: tracking.invoice_number
                        }
                    }
                "#,
                );

            ctx.transition("human_rejected", "Human Rejected")
                .process_step_completed("review")
                .read_input("process", &processes)
                .auto_input("tracking", &review_tracking)
                .auto_input("timer", &timer_scheduled)
                .auto_input("response", &sig_review_response)
                .guard(
                    r#"response.task_id == tracking.task_id && timer.payload.task_id == tracking.task_id && response.data.decision != "approve""#,
                )
                .auto_output("rejected", &rejected)
                .auto_output("cancel", &timer_to_cancel)
                .auto_output("process", &process_done)
                .logic(
                    r#"
                    #{
                        rejected: #{
                            invoice_number: tracking.invoice_number,
                            rejected_by: tracking.reviewer,
                            reason: if response.data.comments == () { "Rejected without comment" } else { response.data.comments }
                        },
                        cancel: #{
                            timer_correlation_id: timer.timer_correlation_id,
                            target_place_id: timer.target_place_id
                        },
                        process: #{
                            outcome: "rejected",
                            invoice_number: tracking.invoice_number
                        }
                    }
                "#,
                );

            ctx.transition("sla_breach", "SLA Breach - Escalate")
                .auto_input("tracking", &review_tracking)
                .auto_input("timer", &timer_scheduled)
                .auto_input("timeout", &sig_sla_timeout)
                .guard(
                    r#"timeout.task_id == tracking.task_id && timer.payload.task_id == tracking.task_id"#,
                )
                .auto_output("escalated", &escalated)
                .auto_output("cancel", &task_to_cancel)
                .auto_output("process", &process_done)
                .logic(
                    r#"
                    #{
                        escalated: #{
                            invoice_number: tracking.invoice_number,
                            original_reviewer: tracking.reviewer,
                            escalation_reason: "SLA breach — no review within 5-minute deadline"
                        },
                        cancel: #{
                            task_id: tracking.task_id,
                            place: "sig_review_response",
                            reason: "SLA breach — auto-escalated after 5-minute deadline"
                        },
                        process: #{
                            outcome: "escalated",
                            invoice_number: tracking.invoice_number
                        }
                    }
                "#,
                );
        });

        // ── Escalation ───────────────────────────────────────────────
        ctx.scope("Escalation", |ctx| {
            let task_cancelled = ctx.state::<HumanTaskCancelled>("task_cancelled", "Cancelled Tasks");

            ctx.transition("cancel_review_task", "Cancel Review Task")
                .human_cancel_to(HumanTaskCancel {
                    task: &task_to_cancel,
                    cancelled: &task_cancelled,
                    errors: &effect_errors,
                });
        });
    });
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    aithericon_sdk::run(
        "invoice-processing-orchestrator",
        "Invoice Processing Pipeline: Vision OCR / Kreuzberg + Python validation + file-ops + LLM + human review + timer (dynamic child net spawning)",
        definition,
    );
}
