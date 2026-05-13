# ADR 001: Universal Proxying Mesh & Dynamic Service Reporting

## Status
Proposed

## Context
The current `aithericon-executor` architecture is optimized for "run-to-completion" batch jobs. However, many workloads (e.g., interactive simulations, debuggers, long-running web services) require external network access.

To support these "Service Workloads," we face several challenges:
1.  **Dynamic Networking:** Processes are assigned random high ports or run inside containers/sandboxes with dynamic IP addresses.
2.  **Service Discovery:** We lack a mechanism to broadcast "I am running on Host X, Port Y" to the outside world.
3.  **Infrastructure Agnostic:** The solution must work across local processes, Docker containers, and potentially cross-cloud setups (via NAT) without heavy external dependencies like Consul or Kubernetes Ingress.
4.  **Mid-Process Changes:** A process might not know its port at startup, or might open multiple ports dynamically during its lifecycle (e.g., a training job opening a TensorBoard instance halfway through).

## Decision

We will implement a **Universal Proxying Mesh** leveraging the existing NATS JetStream backbone for the control plane and **Pingora** (Cloudflare's Rust framework) for the data plane.

### 1. NATS-Driven Control Plane
Instead of a separate service registry, we will use the executor's **Event Stream** as the source of truth.

*   **Service Events:** We will introduce a new `Service` event category and a `ReportService` IPC method.
*   **Routing Table:** The proxy will build its routing table passively by subscribing to `executor.events.>.service` and `executor.status.>`.

### 2. The `executor-proxy` Service
We will create a new crate, `executor-proxy`, built on Pingora.
*   **Ingress:** Listens on HTTP/HTTPS (e.g., `*.lab.local`).
*   **Dynamic Routing:** Maps `execution_id` (subdomain) to the target worker's `host:port`.
*   **Tunneling (Future):** Pingora will eventually support accepting incoming connections from workers (reverse tunneling) to solve NAT traversal, allowing workers to "dial out" to the proxy.

### 3. Worker Modifications
We will enhance the `executor-worker` and `ipc-sidecar` to support network awareness.

*   **Static Port Assignment:** A `PortAssignmentHook` (Staging Hook) will find free ports before execution and inject them as environment variables (e.g., `AITHERICON_SERVICE_PORT`).
*   **Dynamic Reporting (IPC):** The IPC Sidecar will expose a `ReportService` RPC. This allows the child process (or a wrapper) to signal: "I just opened port 8080 for protocol 'http'".
*   **Event Publication:** These signals are published to NATS as `ServiceUpdated` events, which the proxy consumes to update its routing table immediately.

## Detailed Design

### Domain Model Changes
**`executor-domain`**:
*   Add `EventCategory::Service`.
*   Add `StatusDetail::ServiceUpdated` with fields: `name`, `port`, `protocol`, `up` (bool), `metadata`.

**`executor-ipc`**:
*   Add `ReportService` RPC to `executor_sidecar.proto`.

### Workflow
1.  **Start:** Executor starts. `PortAssignmentHook` reserves Port 1234.
2.  **Launch:** Child process starts with `PORT=1234`.
3.  **Advertise (Initial):** Executor publishes `Running` status. Proxy routes `job-id.lab.local` -> `worker-ip:1234`.
4.  **Dynamic Update:** Child process starts a debugger on Port 9000.
5.  **Signal:** Child calls `IPC.ReportService("debugger", 9000)`.
6.  **Event:** Sidecar publishes `ServiceUpdated` event.
7.  **Re-Route:** Proxy adds route `debugger-job-id.lab.local` -> `worker-ip:9000`.

## Consequences

### Positive
*   **Zero-Config:** No manual DNS or load balancer configuration required.
*   **Single Backbone:** Reuses NATS; no new infrastructure (etcd/consul) needed.
*   **Real-Time:** Routes appear as soon as the process reports them.
*   **Unified Stack:** Pure Rust solution (Pingora + Tokio).

### Negative
*   **Complexity:** Adds a new distributed system component (`executor-proxy`).
*   **Dependency:** Workers must explicitly report ports via IPC if they don't use the pre-assigned environment variables.

### Risks
*   **Security:** Exposing internal worker ports to the proxy implies trust. We must ensure the proxy properly authenticates requests (e.g., via checking `petri-lab` permissions).
*   **NAT Traversal:** The initial "Flat Network" implementation assumes the proxy can dial the worker IP. Cross-cloud scenarios will require the Tunneling implementation (Phase 2).
