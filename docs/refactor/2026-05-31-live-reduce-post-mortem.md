# Post-Mortem: Live IPC Reducer Implementation (LiveReduce)

Date: 2026-05-31  
Status: Reverted from `main`  
Feature: `stream_consumer` (LiveReduce mode)

## Overview

The `LiveReduce` dispatch mode for `StreamConsumer` nodes was intended to provide a high-performance, long-lived Python reducer that receives data chunks live over an IPC sidecar as they are produced by upstream nodes. While the `SequentialBody` and `ParallelBody` modes (per-chunk ephemeral jobs) are architecturally sound, the `LiveReduce` implementation contained several critical architectural flaws that led to deadlocks and incorrect behavior.

## Discovered Flaws

### 1. The Sparse Sequence Wedge (Critical Deadlock)
The executor-side `ReorderBuffer` (in `executor-worker/src/chunks.rs`) is designed to ensure lossless, ordered delivery to the Python child process. It does this by strictly expecting a **dense** sequence (`next, next + 1, ...`).

**The Flaw:** The compiler lowering for `LiveReduce` fed chunks to the executor using the producer's raw `chunk.sequence`. This sequence is **sparse**—it is a global atomic counter in the producer's `StreamContext` that increments on *every* event, including logs, metrics, and progress updates.

**Impact:** If a producer logs a single message between two `set_output` calls, the chunks arrive at the reducer with a gap (e.g., sequences `0` and `2`). The `ReorderBuffer` will hang forever waiting for sequence `1`, wedging the Python `aithericon.chunks()` generator and the entire workflow.

### 2. The EOF Sequence Collision
The implementation used the producer's final `stream_count` (the total number of chunks `N`) as the sequence number for the EOF sentinel chunk.

**The Flaw:** In a sparse sequence environment, the raw sequence numbers can easily exceed `N`. 
**Impact:** If a producer emits 5 chunks with sparse sequences (e.g., `0, 2, 4, 7, 8`), sending an EOF with sequence `5` causes the `ReorderBuffer` to release the EOF sentinel *before* chunks `7` and `8`. The Python reducer terminates prematurely, losing data.

### 3. The 0-Chunk Bootstrapping Deadlock
The `LiveReduce` reducer job was designed to start only when the **first chunk** arrived (via `t_<id>_start_reducer`).

**The Flaw:** If an upstream producer completes without ever calling `set_output` (`stream_count = 0`), the "start" transition never fires.
**Impact:** The producer's terminal control token arrives and tries to trigger `t_<id>_eof` to shut down the reducer. However, `t_eof` requires a read-arc on the reducer's `execution_id` (parked at `p_exec_id`). Since the job never started, `p_exec_id` is empty, the EOF transition can never fire, and the workflow deadlocks in a non-terminal state.

### 4. First-Chunk Duplication
To bootstrap the reducer, the first chunk was injected into the reducer's initial `input.json` AND re-emitted to the stream to be caught by the IPC feed.

**The Flaw:** This forces the Python script to receive the first chunk twice.
**Impact:** Script authors are forced to implement manual deduplication or ignore the standard `input` wrapper entirely, creating a confusing and non-idiomatic SDK experience.

### 5. Schema Leakage and Boilerplate
The `feed_chunks: bool` flag was added directly to the generic `AutomatedStep` variant in `template.rs`.

**The Flaw:** `feed_chunks` is an execution-layer detail specific to the `LiveReduce` dispatch mode. Adding it to the core `WorkflowNodeData` leaked internal implementation details into the generic AST.
**Impact:** This forced the manual update of hundreds of test initializers across the codebase to propagate a `false` flag, increasing maintenance burden and polluting the data model.

## Proposed Resolution

To successfully re-implement `LiveReduce`, the following architectural changes are required:

1.  **Dense Renumbering:** The `StreamConsumer` must maintain its own dense arrival counter (similar to `p_ingest` in `SequentialBody`) to re-sequence chunks `0..N-1` before sending them to the executor.
2.  **Immediate Bootstrapping:** The reducer job must start immediately upon node entry, regardless of whether chunks have arrived yet. This ensures the `execution_id` is always available for the EOF sentinel.
3.  **Clean EOF:** The EOF sentinel should be sent with sequence `N` (using the dense counter), guaranteeing it is always the final message released by the buffer.
4.  **Refined Data Model:** The `feed_chunks` flag should be moved into a more appropriate location (e.g., inside `ExecutionSpecConfig` or as a property of the `DeploymentModel`) to avoid polluting the top-level node data.
5.  **SDK Consistency:** The SDK should either receive *all* chunks via IPC or *all* chunks via the initial token, but never a mix that requires manual deduplication.
