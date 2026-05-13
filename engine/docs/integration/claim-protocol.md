# Claim Protocol: Cross-Net Resource Coordination

> Multi-layer Petri nets coordinating resource access through claims.

## Overview

The claim protocol enables **workflows to claim resources** from **adapter nets** with proper lifecycle handling:

- Workflows hold **references** (ClaimHandles), not actual resources
- Adapters own **resource truth** and grant/deny/invalidate claims
- Single global stream (`PETRI_GLOBAL`) provides total ordering
- Invalidation notifications trigger **compensation** in workflows

```
┌─────────────────────┐                        ┌─────────────────────┐
│    WORKFLOW NET     │                        │    ADAPTER NET      │
│                     │                        │                     │
│  ┌───────────────┐  │  1. ClaimRequest       │  ┌───────────────┐  │
│  │ pending_claim │──│───────────────────────>│  │ ClaimRequest  │  │
│  └───────────────┘  │  petri.claims.request  │  │  Listener     │  │
│                     │                        │  └───────┬───────┘  │
│                     │                        │          │          │
│  ┌───────────────┐  │  2. ClaimGranted       │          ▼          │
│  │ claim_handles │<─│────────────────────────│  ┌───────────────┐  │
│  │ (ClaimHandle) │  │  petri.claims.granted  │  │ grant/deny    │  │
│  └───────────────┘  │                        │  └───────────────┘  │
│                     │                        │                     │
│  ┌───────────────┐  │  4. ClaimInvalidated   │                     │
│  │  compensation │<─│────────────────────────│  (resource dies)    │
│  │   triggered   │  │ petri.claims.invalid   │                     │
│  └───────────────┘  │                        │                     │
└─────────────────────┘                        └─────────────────────┘
```

---

## Domain Types

### ClaimHandle (Workflow Side)

A reference held by the workflow pointing to a claimed resource:

```rust
pub struct ClaimHandle {
    /// Unique identifier for this claim handle
    pub id: ClaimHandleId,
    /// Which adapter net owns the resource
    pub resource_net_id: Uuid,
    /// Resource ID within the adapter net
    pub resource_id: String,
    /// Snapshot of resource data at claim time
    pub resource_data_snapshot: serde_json::Value,
    /// When the claim was acquired
    pub acquired_at: DateTime<Utc>,
    /// Current state: Valid | Invalidated | Released
    pub state: ClaimHandleState,
}
```

### ClaimRef (Adapter Side)

Stored in the adapter's claimed tokens, tracking who holds the claim:

```rust
pub struct ClaimRef {
    /// Matches the ClaimHandle.id in the workflow
    pub handle_id: ClaimHandleId,
    /// Which workflow net holds this claim
    pub workflow_net_id: Uuid,
    /// Which workflow instance holds this claim
    pub workflow_id: Uuid,
    /// When the claim was granted
    pub granted_at: DateTime<Utc>,
    /// Optional expiration time
    pub expires_at: Option<DateTime<Utc>>,
}
```

---

## Protocol Messages

### Request Flow (Workflow → Adapter)

```rust
/// Workflow requests a resource
pub struct ClaimRequest {
    pub request_id: Uuid,
    pub resource_net_id: Uuid,
    pub selector: ResourceSelector,  // Any | ById | ByFilter
    pub workflow_net_id: Uuid,
    pub workflow_id: Uuid,
    pub timestamp: DateTime<Utc>,
}

/// Workflow releases a claim
pub struct ClaimReleased {
    pub handle_id: ClaimHandleId,
    pub workflow_net_id: Uuid,
    pub workflow_id: Uuid,
    pub timestamp: DateTime<Utc>,
}
```

### Response Flow (Adapter → Workflow)

```rust
/// Adapter grants a claim
pub struct ClaimGranted {
    pub request_id: Uuid,
    pub claim_handle: ClaimHandle,
    pub claim_ref: ClaimRef,
    pub timestamp: DateTime<Utc>,
}

/// Adapter denies a claim
pub struct ClaimDenied {
    pub request_id: Uuid,
    pub reason: ClaimDeniedReason,  // NoResourcesAvailable | ResourceNotFound | etc.
    pub timestamp: DateTime<Utc>,
}

/// Adapter invalidates a held claim
pub struct ClaimInvalidated {
    pub handle_id: ClaimHandleId,
    pub reason: ClaimInvalidatedReason,  // ResourceDestroyed | Expired | etc.
    pub resource_id: String,
    pub timestamp: DateTime<Utc>,
}
```

---

## Subject Patterns

All claim messages flow through `PETRI_GLOBAL` stream:

```
petri.claims.
├── request.{resource_net_id}               # Workflow → Adapter
├── granted.{workflow_net_id}.{workflow_id}  # Adapter → Workflow
├── denied.{workflow_net_id}.{workflow_id}   # Adapter → Workflow
├── invalidated.{workflow_net_id}.{workflow_id}  # Adapter → Workflow
└── released.{resource_net_id}              # Workflow → Adapter
```

### Subscription Patterns

| Role | Subject Pattern | Purpose |
|------|-----------------|---------|
| Adapter | `petri.claims.request.{my_net_id}` | Receive claim requests |
| Adapter | `petri.claims.released.{my_net_id}` | Receive claim releases |
| Workflow | `petri.claims.granted.{my_net_id}.{my_wf_id}` | Receive grants |
| Workflow | `petri.claims.denied.{my_net_id}.{my_wf_id}` | Receive denials |
| Workflow | `petri.claims.invalidated.{my_net_id}.{my_wf_id}` | Receive invalidations |

---

## Claim Flow

### Happy Path

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  1. WORKFLOW REQUESTS CLAIM                                                  │
│                                                                             │
│  Workflow transition fires → needs a worker                                 │
│         │                                                                   │
│         ▼                                                                   │
│  Publish ClaimRequest                                                       │
│    subject: petri.claims.request.{adapter_net_id}                          │
│    payload: { request_id, selector: Any, workflow_id, ... }                │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  2. ADAPTER GRANTS CLAIM                                                     │
│                                                                             │
│  Adapter receives request                                                   │
│         │                                                                   │
│         ▼                                                                   │
│  Check availability → resource found                                        │
│         │                                                                   │
│         ▼                                                                   │
│  Move token: available → claimed (with ClaimRef)                           │
│         │                                                                   │
│         ▼                                                                   │
│  Publish ClaimGranted                                                       │
│    subject: petri.claims.granted.{wf_net_id}.{wf_id}                       │
│    payload: { claim_handle, claim_ref, ... }                               │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  3. WORKFLOW RECEIVES GRANT                                                  │
│                                                                             │
│  Workflow receives ClaimGranted                                             │
│         │                                                                   │
│         ▼                                                                   │
│  Create ClaimHandle token in claim_handles place                           │
│         │                                                                   │
│         ▼                                                                   │
│  Workflow continues with resource reference                                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Compensation Path (Resource Destroyed)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  4. RESOURCE CRASHES                                                         │
│                                                                             │
│  External system: worker pod crashes                                        │
│         │                                                                   │
│         ▼                                                                   │
│  Adapter detects failure                                                    │
│         │                                                                   │
│         ▼                                                                   │
│  Look up ClaimRef for destroyed resource                                   │
│         │                                                                   │
│         ▼                                                                   │
│  Publish ClaimInvalidated                                                   │
│    subject: petri.claims.invalidated.{wf_net_id}.{wf_id}                   │
│    payload: { handle_id, reason: ResourceDestroyed, ... }                  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  5. WORKFLOW COMPENSATES                                                     │
│                                                                             │
│  Workflow receives ClaimInvalidated                                         │
│         │                                                                   │
│         ▼                                                                   │
│  Inject signal token to invalidation_signal place                          │
│         │                                                                   │
│         ▼                                                                   │
│  Compensation transition fires (e.g., retry with different resource)       │
│         │                                                                   │
│         ▼                                                                   │
│  Workflow recovers or fails gracefully                                      │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Implementation

### Adapter Side

```rust
use petri_nats::{
    ClaimPublisher, ClaimRequestListener, ClaimRequestHandler,
    SimpleClaimHandler, create_claim_request_consumer,
};

// Create publisher for sending grants/denials/invalidations
let publisher = Arc::new(ClaimPublisher::new(jetstream.clone()));

// Create handler with resource pool
let handler = Arc::new(SimpleClaimHandler::new(adapter_net_id, publisher.clone()));

// Add resources to the pool
handler.add_resource("worker-1", json!({"cpu": 4, "memory": 16}));
handler.add_resource("worker-2", json!({"cpu": 8, "memory": 32}));

// Create consumer for claim requests
let consumer = create_claim_request_consumer(
    &jetstream,
    adapter_net_id,
    "my-adapter-claims",
).await?;

// Run the listener
let listener = ClaimRequestListener::new(consumer, handler);
tokio::spawn(listener.run());
```

### Workflow Side

```rust
use petri_nats::{
    ClaimPublisher, ClaimResponseListener, TokenInjectingClaimHandler,
    create_claim_response_consumer,
};

// Create publisher for sending requests/releases
let publisher = Arc::new(ClaimPublisher::new(jetstream.clone()));

// Create handler that injects tokens into the workflow
let handler = Arc::new(
    TokenInjectingClaimHandler::new(service.clone(), claim_handle_place_id)
        .with_invalidation_place(invalidation_signal_place_id)
        .with_eval_notify(eval_notify.clone()),
);

// Create consumer for claim responses
let consumer = create_claim_response_consumer(
    &jetstream,
    workflow_net_id,
    workflow_id,
    &format!("workflow-claims-{}", workflow_id),
).await?;

// Run the listener
let listener = ClaimResponseListener::new(
    consumer,
    handler,
    workflow_net_id,
    workflow_id,
);
tokio::spawn(listener.run());

// Request a claim
let request = ClaimRequest::new(
    adapter_net_id,
    ResourceSelector::Any,
    workflow_net_id,
    workflow_id,
);
publisher.publish_request(&request).await?;
```

---

## Handlers

### SimpleClaimHandler (Built-in)

In-memory handler for testing/demo:

```rust
let handler = SimpleClaimHandler::new(adapter_net_id, publisher);

// Add resources
handler.add_resource("r1", json!({"type": "gpu"}));

// Remove resources (triggers invalidation for any claims)
handler.remove_resource("r1");

// Implements ClaimRequestHandler trait
// - Grants claims on first-come-first-served basis
// - Publishes grants/denials automatically
```

### TokenInjectingClaimHandler (Production)

Integrates with PetriNetService:

```rust
let handler = TokenInjectingClaimHandler::new(service, claim_handle_place)
    .with_denial_place(denial_signal_place)
    .with_invalidation_place(invalidation_signal_place)
    .with_eval_notify(notify);

// On grant: creates ClaimHandle token in claim_handle_place
// On denial: injects signal to denial_signal_place
// On invalidation: injects signal to invalidation_signal_place
```

### Custom Handler

Implement `ClaimRequestHandler` for custom logic:

```rust
#[async_trait]
impl ClaimRequestHandler for MyAdapter {
    async fn handle_request(
        &self,
        request: ClaimRequest,
    ) -> Result<ClaimGranted, ClaimDenied> {
        // Custom resource selection logic
        let resource = match &request.selector {
            ResourceSelector::Any => self.find_any_available(),
            ResourceSelector::ById { resource_id } => self.find_by_id(resource_id),
            ResourceSelector::ByFilter { filter } => self.find_by_filter(filter),
        }?;

        // Grant the claim
        let claim_handle = ClaimHandle::new(
            self.adapter_net_id,
            &resource.id,
            resource.data.clone(),
        );
        let claim_ref = ClaimRef::new(
            claim_handle.id,
            request.workflow_net_id,
            request.workflow_id,
        );

        // Publish grant
        self.publisher.publish_granted(&ClaimGranted {
            request_id: request.request_id,
            claim_handle,
            claim_ref,
            timestamp: Utc::now(),
        }).await?;

        Ok(granted)
    }
}
```

---

## Workflow Scenario Integration

Define places for claim handling:

```rust
use aithericon_sdk::prelude::*;

fn define_workflow(ctx: &mut Context) {
    // Place for ClaimHandle tokens
    let claim_handles = ctx.resource::<ClaimHandleToken>("claim_handles", "Claimed Resources");

    // Signal place for invalidations (triggers compensation)
    let invalidation_signal = ctx.signal::<InvalidationSignal>(
        "sig_invalidation",
        "Resource Invalidation Signals"
    );

    // Processing state
    let processing = ctx.state::<JobToken>("processing", "Processing Jobs");

    // Compensation transition
    ctx.transition("handle_invalidation", "Handle Resource Loss")
        .auto_input("job", &processing)
        .auto_input("signal", &invalidation_signal)
        .auto_output("retry", &pending_jobs)  // Move job back to pending
        .logic(r#"#{ retry: job }"#)
        .build();
}
```

---

## Best Practices

### 1. Track ClaimRefs for Invalidation

```rust
// Adapter should maintain: resource_id → Vec<ClaimRef>
// On resource destruction, notify all claim holders
async fn on_resource_destroyed(&self, resource_id: &str) {
    if let Some(claim_refs) = self.claims_by_resource.get(resource_id) {
        for claim_ref in claim_refs {
            self.publisher.publish_invalidated(
                claim_ref.workflow_net_id,
                claim_ref.workflow_id,
                &ClaimInvalidated {
                    handle_id: claim_ref.handle_id,
                    reason: ClaimInvalidatedReason::ResourceDestroyed,
                    resource_id: resource_id.to_string(),
                    timestamp: Utc::now(),
                },
            ).await?;
        }
    }
}
```

### 2. Set Claim Expiration

```rust
// Prevent resource leaks from crashed workflows
let claim_ref = ClaimRef::new(handle_id, wf_net_id, wf_id)
    .with_expiration(Utc::now() + chrono::Duration::minutes(30));

// Periodic expiration check
async fn check_expired_claims(&self) {
    for (resource_id, claim_refs) in &self.claims {
        for claim_ref in claim_refs {
            if claim_ref.is_expired() {
                self.invalidate_claim(claim_ref, ClaimInvalidatedReason::Expired).await;
            }
        }
    }
}
```

### 3. Handle Concurrent Requests

```rust
// Use atomic operations for resource allocation
let resource = {
    let mut pool = self.pool.write();
    pool.pop()  // Atomically remove from available
};

// If grant fails, return resource to pool
if publish_failed {
    self.pool.write().push(resource);
}
```

### 4. Log Claim Lifecycle

```rust
info!(
    request_id = %request.request_id,
    workflow_id = %request.workflow_id,
    selector = ?request.selector,
    "Processing claim request"
);

info!(
    handle_id = %handle.id,
    resource_id = %handle.resource_id,
    workflow_id = %claim_ref.workflow_id,
    "Granted claim"
);
```

---

## Comparison with Direct Token Injection

| Aspect | Claim Protocol | Direct Injection |
|--------|----------------|------------------|
| **Ownership** | Explicit ClaimHandle/ClaimRef |
| **Invalidation** | Adapter notifies claim holder | Signal routing via claimed metadata |
| **Coordination** | Request/Grant/Deny/Release | Direct create/update/remove |
| **Use Case** | Exclusive resource access | Shared pool access |

Use the **claim protocol** when:
- Resources need exclusive ownership
- Workflows must be notified of resource loss
- Cross-net coordination is required

Use **direct injection** when:
- Resources are shared/pooled
- No exclusive ownership needed
- Simple create/update/remove is sufficient
