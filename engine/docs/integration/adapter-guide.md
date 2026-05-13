# Building Resource Adapters

> A practical guide to building adapters using the generic primitives.

## What is an Adapter?

An adapter bridges external systems with the Petri-Lab engine by:

1. **Modifying shared places** - Create/update/remove tokens based on external state
2. **Handling export commands** - React when workflows export commands
3. **Injecting signals** - Send responses to specific workflows

Adapters are external processes communicating via NATS. They use **generic primitives**, not specialized patterns.

---

## The Primitives (Adapter Perspective)

### What Adapters Do

| Action | Primitive | Subject Pattern |
|--------|-----------|-----------------|
| Create token | Inject to shared place | `petri.resource.{type}.inject.{place}` |
| Update token | Update in shared place | `petri.resource.{type}.update.{place}` |
| Remove token | Remove from shared place | `petri.resource.{type}.remove.{place}` |
| Receive command | Subscribe to export place | `petri.resource.{type}.command.{place}` |
| Send response | Inject to signal place | `petri.resource.{type}.inject.{signal_place}` |

### Simple Model

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         ADAPTER INTERACTIONS                                 │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  SHARED PLACES (adapter manages pool state)                          │   │
│  │                                                                      │   │
│  │  workers/available                                                   │   │
│  │    ← inject: Create token (external resource appeared)              │   │
│  │    ← update: Update token (resource state changed)                  │   │
│  │    ← remove: Remove token (resource gone)                           │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  EXPORT PLACES (adapter receives commands)                           │   │
│  │                                                                      │   │
│  │  workers/request                                                     │   │
│  │    → command: Workflow wants something (subscribe to this)          │   │
│  │    → payload includes workflow_id (for response routing)            │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  SIGNAL PLACES (adapter sends responses)                             │   │
│  │                                                                      │   │
│  │  sig_reservation                                                     │   │
│  │    ← inject: Send response to workflow (include workflow_id)        │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Basic Adapter Structure

```rust
use async_nats::jetstream;
use serde::{Deserialize, Serialize};

#[tokio::main]
async fn main() -> Result<()> {
    // Connect to NATS
    let client = async_nats::connect("nats://localhost:4333").await?;
    let jetstream = async_nats::jetstream::new(client);

    // Start adapter components
    tokio::select! {
        // Observe external system, inject to shared places
        _ = run_observer(&jetstream) => {},

        // Handle commands from export places
        _ = run_command_handler(&jetstream) => {},
    }

    Ok(())
}
```

---

## Injecting to Shared Places

### Creating Tokens

When external resources appear (pod created, job submitted, etc.):

```rust
async fn inject_token(
    jetstream: &JetStream,
    resource_type: &str,
    place: &str,
    token_data: serde_json::Value,
) -> Result<()> {
    let subject = format!("petri.resource.{}.inject.{}", resource_type, place);

    let payload = serde_json::json!({
        "place_name": place,
        "token_data": token_data,
        "idempotency_key": token_data["id"],  // For dedup
        "timestamp": chrono::Utc::now(),
    });

    jetstream.publish(subject, payload.to_string().into()).await?;
    Ok(())
}

// Usage
inject_token(&jetstream, "workers", "available", json!({
    "id": "worker-1",
    "node": "node-1",
    "memory": 16384,
})).await?;
```

### Updating Tokens

When external resource state changes:

```rust
async fn update_token(
    jetstream: &JetStream,
    resource_type: &str,
    place: &str,
    token_id: &str,
    new_data: serde_json::Value,
) -> Result<()> {
    let subject = format!("petri.resource.{}.update.{}", resource_type, place);

    let payload = serde_json::json!({
        "place_name": place,
        "token_id": token_id,
        "token_data": new_data,
        "timestamp": chrono::Utc::now(),
    });

    jetstream.publish(subject, payload.to_string().into()).await?;
    Ok(())
}
```

### Removing Tokens

When external resources disappear:

```rust
async fn remove_token(
    jetstream: &JetStream,
    resource_type: &str,
    place: &str,
    token_id: &str,
    reason: Option<&str>,
) -> Result<()> {
    let subject = format!("petri.resource.{}.remove.{}", resource_type, place);

    let payload = serde_json::json!({
        "place_name": place,
        "token_id": token_id,
        "reason": reason,
        "timestamp": chrono::Utc::now(),
    });

    jetstream.publish(subject, payload.to_string().into()).await?;
    Ok(())
}

// Note: If token was in a CLAIMED place, engine will route
// a signal to the owning workflow automatically
```

---

## Handling Export Commands

### Subscribing to Commands

```rust
async fn run_command_handler(jetstream: &JetStream) -> Result<()> {
    // Subscribe to all commands for this resource type
    let consumer = jetstream
        .get_stream("PETRI_RESOURCE_workers")
        .await?
        .create_consumer(consumer::pull::Config {
            durable_name: Some("worker-adapter".to_string()),
            filter_subject: "petri.resource.workers.command.>".to_string(),
            ..Default::default()
        })
        .await?;

    let mut messages = consumer.messages().await?;

    while let Some(msg) = messages.next().await {
        let msg = msg?;

        // Parse command
        let command: Command = serde_json::from_slice(&msg.payload)?;

        // Handle based on place (command type)
        match command.place_name.as_str() {
            "request" => handle_request(&jetstream, &command).await?,
            "confirm" => handle_confirm(&jetstream, &command).await?,
            "release" => handle_release(&jetstream, &command).await?,
            _ => warn!("Unknown command: {}", command.place_name),
        }

        msg.ack().await?;
    }

    Ok(())
}

#[derive(Deserialize)]
struct Command {
    workflow_id: String,
    workflow_sequence: u64,
    place_name: String,
    token_id: String,
    token_data: serde_json::Value,
    timestamp: DateTime<Utc>,
}
```

### Responding to Commands

Send response to workflow's signal place:

```rust
async fn send_response(
    jetstream: &JetStream,
    resource_type: &str,
    signal_place: &str,
    workflow_id: &str,
    response_data: serde_json::Value,
) -> Result<()> {
    // Inject to signal place, scoped to workflow
    let subject = format!("petri.resource.{}.inject.{}", resource_type, signal_place);

    let payload = serde_json::json!({
        "place_name": signal_place,
        "workflow_id": workflow_id,  // Route to specific workflow
        "token_data": response_data,
        "timestamp": chrono::Utc::now(),
    });

    jetstream.publish(subject, payload.to_string().into()).await?;
    Ok(())
}

// Example: Respond to reservation request
async fn handle_request(jetstream: &JetStream, command: &Command) -> Result<()> {
    let worker_id = command.token_data["id"].as_str().unwrap();

    // Check availability, do reservation logic...
    let accepted = check_and_reserve(worker_id).await?;

    // Send response
    let response = if accepted {
        json!({
            "resource_id": worker_id,
            "status": "confirmed",
            "expires_at": (Utc::now() + Duration::from_secs(60)),
        })
    } else {
        json!({
            "resource_id": worker_id,
            "status": "rejected",
            "reason": "insufficient capacity",
        })
    };

    send_response(
        jetstream,
        "workers",
        "sig_reservation",  // Signal place defined in scenario
        &command.workflow_id,
        response,
    ).await
}
```

---

## Observing External Systems

### Example: Kubernetes Pod Watcher

```rust
async fn run_observer(jetstream: &JetStream) -> Result<()> {
    let client = kube::Client::try_default().await?;
    let pods: Api<Pod> = Api::namespaced(client, "worker-pool");

    let watcher = watcher(pods, ListParams::default());

    tokio::pin!(watcher);

    while let Some(event) = watcher.try_next().await? {
        match event {
            Event::Applied(pod) => {
                // Pod created or updated
                if pod.status.phase == Some("Running".to_string()) {
                    inject_token(jetstream, "workers", "available", json!({
                        "id": pod.metadata.name,
                        "node": pod.spec.node_name,
                        "memory": extract_memory(&pod),
                    })).await?;
                }
            }
            Event::Deleted(pod) => {
                // Pod deleted - remove from wherever it is
                // Engine handles claimed routing automatically
                for place in ["available", "leased", "reserving"] {
                    remove_token(
                        jetstream,
                        "workers",
                        place,
                        &pod.metadata.name,
                        Some("pod deleted"),
                    ).await?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}
```

### Example: Database Job Queue

```rust
async fn run_job_observer(jetstream: &JetStream, db: &Pool) -> Result<()> {
    loop {
        // Poll for new jobs
        let new_jobs = sqlx::query_as!(Job,
            "SELECT * FROM jobs WHERE status = 'pending' AND injected = false"
        ).fetch_all(db).await?;

        for job in new_jobs {
            inject_token(jetstream, "jobs", "pending", json!({
                "id": job.id,
                "job_type": job.job_type,
                "payload": job.payload,
                "priority": job.priority,
            })).await?;

            // Mark as injected
            sqlx::query!("UPDATE jobs SET injected = true WHERE id = $1", job.id)
                .execute(db).await?;
        }

        // Poll for cancelled jobs
        let cancelled = sqlx::query_as!(Job,
            "SELECT * FROM jobs WHERE status = 'cancelled' AND notified = false"
        ).fetch_all(db).await?;

        for job in cancelled {
            // Remove from any place - engine routes signal if claimed
            remove_token(jetstream, "jobs", "claimed", &job.id, Some("user cancelled")).await?;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
```

---

## Patterns Emerge from Primitives

### Pattern: Two-Phase Commit

Not special code - just export places + signal places:

```rust
// Adapter subscribes to export places
let commands = ["request", "confirm", "release"];
for cmd in commands {
    subscribe(format!("petri.resource.workers.command.{}", cmd));
}

// Handle each command type
async fn handle_request(cmd: &Command) -> Result<()> {
    let reserved = reserve_resource(&cmd.token_data).await?;
    send_response("sig_reservation", &cmd.workflow_id, json!({
        "status": if reserved { "confirmed" } else { "rejected" },
        "resource_id": cmd.token_data["id"],
    })).await
}

async fn handle_confirm(cmd: &Command) -> Result<()> {
    confirm_reservation(&cmd.token_data["id"]).await?;
    // No response needed - workflow already moved to leased
    Ok(())
}

async fn handle_release(cmd: &Command) -> Result<()> {
    release_resource(&cmd.token_data["id"]).await?;
    Ok(())
}
```

### Pattern: Fire-and-Forget

Just export places, no response:

```rust
// Subscribe to notification export
subscribe("petri.resource.notifications.command.send");

async fn handle_send(cmd: &Command) -> Result<()> {
    // Send notification to external system
    send_email(&cmd.token_data).await?;
    // No response - workflow continues
    Ok(())
}
```

### Pattern: Pub/Sub

Just inject to shared places, workflows subscribe via transitions:

```rust
// Adapter publishes events as tokens
inject_token(&jetstream, "events", "stream", json!({
    "event_type": "price_update",
    "symbol": "AAPL",
    "price": 150.25,
})).await?;

// Any workflow with a transition consuming from events/stream
// will receive these tokens
```

---

## State Snapshots

Publish periodic snapshots for recovery:

```rust
async fn publish_snapshot(jetstream: &JetStream, resource_type: &str) -> Result<()> {
    // Get current state from your system
    let resources = get_all_resources().await?;

    let snapshot = json!({
        "resource_type": resource_type,
        "timestamp": Utc::now(),
        "tokens": resources.iter().map(|r| json!({
            "place": r.state,
            "token_id": r.id,
            "token_data": r.data,
            "claimed_by": r.claimed_by,
        })).collect::<Vec<_>>(),
    });

    let subject = format!("petri.resource.{}.snapshot", resource_type);
    jetstream.publish(subject, snapshot.to_string().into()).await?;

    Ok(())
}

// Run periodically
async fn snapshot_loop(jetstream: &JetStream) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        if let Err(e) = publish_snapshot(&jetstream, "workers").await {
            error!("Snapshot failed: {}", e);
        }
    }
}
```

---

## Error Handling

### Transient vs Permanent Errors

```rust
async fn handle_command(msg: Message) -> Result<()> {
    let command: Command = serde_json::from_slice(&msg.payload)?;

    match process_command(&command).await {
        Ok(_) => {
            msg.ack().await?;
        }
        Err(e) if e.is_transient() => {
            // Requeue with backoff
            msg.nak_with_delay(Duration::from_secs(5)).await?;
        }
        Err(e) => {
            // Permanent failure - send error response to workflow
            error!("Command failed: {}", e);
            send_response(
                "sig_error",
                &command.workflow_id,
                json!({
                    "error": e.to_string(),
                    "command": command.place_name,
                }),
            ).await?;
            msg.ack().await?;  // Don't retry
        }
    }

    Ok(())
}
```

### Idempotency

Use idempotency keys to handle duplicates:

```rust
async fn inject_token_idempotent(
    jetstream: &JetStream,
    resource_type: &str,
    place: &str,
    idempotency_key: &str,
    token_data: serde_json::Value,
) -> Result<()> {
    // Engine deduplicates based on idempotency_key
    let payload = serde_json::json!({
        "place_name": place,
        "token_data": token_data,
        "idempotency_key": idempotency_key,
        "timestamp": chrono::Utc::now(),
    });

    // Even if this is called multiple times, only one token created
    jetstream.publish(subject, payload.to_string().into()).await?;
    Ok(())
}
```

---

## Testing

### Unit Testing Command Handlers

```rust
#[tokio::test]
async fn test_request_handler() {
    let command = Command {
        workflow_id: "wf_123".to_string(),
        workflow_sequence: 1,
        place_name: "request".to_string(),
        token_id: "tok_1".to_string(),
        token_data: json!({"id": "worker-1"}),
        timestamp: Utc::now(),
    };

    let response = handle_request(&command).await.unwrap();

    assert_eq!(response["status"], "confirmed");
    assert_eq!(response["resource_id"], "worker-1");
}
```

### Integration Testing with NATS

```rust
#[tokio::test]
async fn test_full_flow() {
    // Use test prefix for isolation
    let prefix = format!("test_{}", uuid::Uuid::new_v4());

    // Start adapter with prefix
    let adapter = spawn_adapter_with_prefix(&prefix).await;

    // Inject a worker
    let inject_subject = format!("petri.{}.resource.workers.inject.available", prefix);
    jetstream.publish(inject_subject, worker_payload()).await?;

    // Simulate workflow command
    let command_subject = format!("petri.{}.resource.workers.command.request", prefix);
    jetstream.publish(command_subject, command_payload()).await?;

    // Wait for response
    let response = wait_for_message(
        &format!("petri.{}.resource.workers.inject.sig_reservation", prefix),
        Duration::from_secs(5),
    ).await?;

    assert!(response["status"] == "confirmed");
}
```

---

## Deployment

### Docker Compose

```yaml
version: '3'
services:
  nats:
    image: nats:latest
    command: -js
    ports:
      - "4333:4222"

  worker-adapter:
    build: ./adapters/worker
    environment:
      NATS_URL: nats://nats:4222
      RESOURCE_TYPE: workers
    depends_on:
      - nats

  engine:
    build: ./engine
    environment:
      NATS_URL: nats://nats:4222
    depends_on:
      - nats
```

### Health Checks

```rust
async fn health_server() {
    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/ready", get(|| async move {
            if nats_connected() && external_connected() {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        }));

    axum::Server::bind(&"0.0.0.0:8080".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}
```

---

## Best Practices

### 1. Use Idempotency Keys

```rust
// Always include idempotency_key for inject/update
json!({
    "idempotency_key": format!("{}:{}", resource_id, version),
    ...
})
```

### 2. Include Timestamps

```rust
// Always include timestamp for ordering/debugging
json!({
    "timestamp": Utc::now(),
    ...
})
```

### 3. Handle All Places for Removal

```rust
// When removing, try all possible places
for place in ["available", "reserved", "leased"] {
    remove_token(jetstream, "workers", place, &id, reason).await?;
}
// Engine ignores if token not in that place
```

### 4. Keep Adapters Stateless

```rust
// Don't cache state in adapter
// Query external system or engine for current state
// This makes adapters horizontally scalable
```

### 5. Log Workflow Context

```rust
// Always log workflow_id for tracing
info!(workflow_id = %command.workflow_id, "Processing command");
```

### 6. Publish Snapshots Regularly

```rust
// Enable fast recovery
tokio::spawn(snapshot_loop(jetstream));
```

---

## Summary

Adapters use **three operations** on **three place types**:

| Operation | Shared Places | Export Places | Signal Places |
|-----------|---------------|---------------|---------------|
| **Inject** | Create tokens | - | Send responses |
| **Update** | Modify tokens | - | - |
| **Remove** | Delete tokens | - | - |
| **Subscribe** | - | Receive commands | - |

All patterns (2PC, lifecycle, pub/sub, saga) emerge from these primitives.
