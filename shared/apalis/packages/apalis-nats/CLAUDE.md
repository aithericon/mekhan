# CLAUDE.md - apalis-nats

This file provides guidance to Claude Code (claude.ai/code) when working with the apalis-nats package.

## Package Overview

apalis-nats is a NATS JetStream backend implementation for the Apalis job processing library. It provides priority-based job queueing, dead letter queue support, and distributed tracing capabilities using NATS JetStream as the underlying message broker.

## Architecture

### Core Components

- **NatsStorage**: Main storage implementation using NATS JetStream
  - Implements priority queues (High, Medium, Low) with separate streams
  - Manages job serialization/deserialization
  - Provides dead letter queue (DLQ) functionality
  - Implements the `Backend` and `Storage` traits from apalis-core

- **NatsContext**: Provides access to the underlying NATS message for manual control
  - Allows manual acknowledgment, negative acknowledgment, or termination
  - Carries OpenTelemetry trace context when `otel` feature is enabled

- **Priority System**: Three-level priority with separate JetStream streams
  - High priority jobs are processed first
  - Prevents starvation with round-robin polling

### Key Files

- `src/lib.rs`: Main module exposing public API and documentation
- `src/storage.rs`: NatsStorage implementation with JetStream integration
- `src/expose.rs`: Trait implementations for worker integration

## Dependencies

- `async-nats` (0.35): Official async NATS client with JetStream support
- `apalis-core`: Core traits and types with `sleep` and `json` features
- `serde` & `serde_json`: Serialization/deserialization
- `chrono`: Timestamp handling
- `futures`: Stream utilities
- `tower`: Middleware support
- `thiserror`: Error handling
- Runtime: Tokio (required)
- Optional: `opentelemetry`, `opentelemetry-nats`, `tracing-opentelemetry` for distributed tracing

## Development

### Building
```bash
# Build the package
cargo build -p apalis-nats

# Build with OpenTelemetry support
cargo build -p apalis-nats --features otel

# Build with all features
cargo build -p apalis-nats --all-features
```

### Testing
```bash
# Run integration tests (uses testcontainers to spin up NATS)
cargo test -p apalis-nats

# Run with test output
cargo test -p apalis-nats -- --nocapture
```

### No Examples Directory
The package doesn't have its own examples directory. Examples are provided in:
- The main README.md documentation
- The doc comments in src/lib.rs
- The root /examples directory of the workspace

## NATS Setup

For local development, you need a NATS server with JetStream enabled:

```bash
# Using Docker (JetStream enabled by default in recent versions)
docker run -d -p 4222:4222 nats:latest -js

# Using NATS CLI with JetStream
nats-server -js

# For testing, testcontainers automatically handles this
```

## Configuration

### Connection Methods
```rust
// Basic connection
let client = apalis_nats::connect("nats://localhost:4222").await?;

// With credentials file
let client = apalis_nats::connect_with_credentials(url, "path/to/my.creds").await?;

// With username/password
let client = apalis_nats::connect_with_user_pass(url, "user", "pass").await?;

// With custom options
let client = apalis_nats::connect_with_options(url, options).await?;
```

### Storage Configuration (Config struct)
- `namespace`: Stream prefix (default: "apalis")
- `max_deliver`: Max retry attempts (default: 5)
- `ack_wait`: Job processing timeout (default: 30s)
- `num_replicas`: Stream replicas (default: 1)
- `enable_dlq`: Dead letter queue (default: true)
- `max_ack_pending`: Max unacked messages (default: 100)
- `enable_tracing`: OpenTelemetry support (default: true with `otel` feature)

## Common Patterns

### Basic Job Publishing
```rust
// Default priority (Medium)
storage.push(job).await?;

// With specific priority
storage.push_with_priority(job, Priority::High).await?;
```

### Worker with Context Access
```rust
async fn process_job(job: MyJob, ctx: Context<NatsContext>) -> Result<(), Error> {
    // Access NATS context for manual control
    if let Some(nats_ctx) = ctx.data_opt::<NatsContext>() {
        // Manual ack/nack/term control
    }
    Ok(())
}
```

## Key Types and Errors

### NatsPollError
- `Client`: NATS client errors
- `JetStream`: JetStream-specific errors
- `Codec`: Serialization/deserialization errors
- `Storage`: Storage operation errors

### NatsQueueInfo
Contains metadata about queue state:
- Active job counts per priority
- DLQ count
- Stream health status

## Stream Organization

JetStream streams created per priority:
- `{namespace}_high`: High priority jobs
- `{namespace}_medium`: Medium priority jobs  
- `{namespace}_low`: Low priority jobs
- `{namespace}_dlq`: Dead letter queue (if enabled)

Workers poll in priority order with brief sleep between checks to prevent CPU spinning.

## Performance Considerations

- JetStream provides persistence and at-least-once delivery
- Flow control prevents consumer overwhelming
- Max ACK pending limits memory usage
- Priority polling ensures high-priority job processing
- Horizontal scaling via multiple worker instances

## Testing Strategy

- Integration tests use testcontainers for NATS JetStream
- Tests cover priority queuing, DLQ, and retry logic
- OpenTelemetry trace propagation tested with `otel` feature
- No unit tests needed due to tight NATS integration

## Feature Flags

- `default`: Empty (no default features)
- `otel`: OpenTelemetry tracing support
