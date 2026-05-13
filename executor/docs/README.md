# Executor Documentation

## Contents

1. [Architecture](architecture.md) — Crate map, data flow, design decisions
2. [Job Model](job-model.md) — ExecutionJob, ProcessSpec, inputs and outputs
3. [Execution Lifecycle](execution-lifecycle.md) — Status state machine, handler flow, staging pipeline
4. [Run Directory](run-directory.md) — Filesystem layout, context.json, environment variables
5. [IPC Protocol](ipc-protocol.md) — FlatBuffers schema, framing, request/response catalog
6. [NATS Contracts](nats-contracts.md) — Subjects, streams, status updates, events
7. [Storage](storage.md) — ArtifactStore trait, local layout, artifact model
8. [Backend Trait](backend-trait.md) — ExecutionBackend interface, implementing a new backend
9. [Configuration](configuration.md) — EXECUTOR_* environment variables and defaults
10. [Workspace Persistence](workspace-persistence.md) — Ephemeral compute, crash recovery, CDC sync, artifact API integration
11. [LLM Vision & OCR](llm-vision.md) — Image inputs, OCR setup, supported models, provider-specific formats

## Reading Paths

**Submitting jobs / building an SDK:**
[Job Model](job-model.md) → [NATS Contracts](nats-contracts.md) → [Run Directory](run-directory.md) → [IPC Protocol](ipc-protocol.md)

**Implementing a new backend:**
[Backend Trait](backend-trait.md) → [Execution Lifecycle](execution-lifecycle.md) → [Run Directory](run-directory.md) → [IPC Protocol](ipc-protocol.md) → [Storage](storage.md)

**Using LLM vision / OCR:**
[LLM Vision & OCR](llm-vision.md) → [Job Model](job-model.md) → [Storage](storage.md)

**Extracting text from documents:**
[Job Model](job-model.md) (Kreuzberg section) → [Backend Trait](backend-trait.md) (KreuzbergBackend) → [Storage](storage.md)

**Deploying on ephemeral compute:**
[Workspace Persistence](workspace-persistence.md) → [Storage](storage.md) → [IPC Protocol](ipc-protocol.md)
