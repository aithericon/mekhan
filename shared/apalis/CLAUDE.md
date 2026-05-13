# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Apalis is a simple, extensible multithreaded background job and message processing library for Rust. It provides task handling with dependency injection similar to actix and axum, leverages the tower ecosystem for middleware, and supports multiple backends (Redis, PostgreSQL, MySQL, SQLite, Cron).

## Architecture

The project follows a workspace structure with the following key components:

- **Core Library** (`/src`): Main apalis crate that re-exports core functionality and provides layers (middleware) for tracing, Sentry, Prometheus, retries, timeouts, and panic catching.

- **Core Package** (`packages/apalis-core`): Foundation package containing:
  - Worker builder and factory patterns
  - Backend traits for job sources
  - Request/response handling
  - Storage abstractions
  - Service functions with dependency injection
  - Monitoring and graceful shutdown

- **Backend Packages**:
  - `apalis-redis`: Redis-based job storage and processing
  - `apalis-sql`: SQL backends (PostgreSQL, MySQL, SQLite)
  - `apalis-cron`: Cron job scheduling

- **Examples** (`/examples`): Comprehensive examples demonstrating various use cases and integrations

## Key Concepts

- **Workers**: Async functions that process jobs, built using `WorkerBuilder`
- **Backends/Storage**: Where jobs are persisted (Redis, SQL databases)
- **Monitor**: Manages multiple workers with graceful shutdown
- **Layers**: Tower middleware for cross-cutting concerns (tracing, metrics, retries)
- **Stepped Tasks**: Beta feature for multi-step job processing

## Development Commands

### Build
```bash
# Build entire workspace
cargo build

# Build with specific features
cargo build --features "limit,tracing,prometheus"

# Build release mode
cargo build --release
```

### Test
```bash
# Run all tests
cargo test

# Run tests for a specific package
cargo test -p apalis-redis
cargo test -p apalis-sql

# Run a specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture
```

### Lint and Format
```bash
# Format code
cargo fmt

# Check formatting without changes
cargo fmt --check

# Run clippy linter
cargo clippy

# Run clippy with all features
cargo clippy --all-features
```

### Examples
```bash
# Run specific example (requires environment setup)
cargo run --example redis
cargo run --example postgres
cargo run --example sqlite

# Most examples require REDIS_URL or DATABASE_URL environment variables
REDIS_URL=redis://localhost cargo run --example redis
```

## Environment Variables

Many examples and tests require:
- `REDIS_URL`: Redis connection string (e.g., `redis://localhost:6379`)
- `DATABASE_URL`: SQL database connection string
- `RUST_LOG`: Logging level (e.g., `debug`, `info`)

## Testing Approach

The codebase uses standard Rust testing with `#[tokio::test]` for async tests. Tests are located alongside source files and in dedicated test modules. Integration tests for backends require corresponding services to be running.