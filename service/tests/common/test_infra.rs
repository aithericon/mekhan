//! Test infrastructure (Postgres + NATS isolation helpers).
//!
//! Previously lived in the external `aithericon-test-infra` crate; inlined
//! here so the cross-musl CI build doesn't have to clone an extra repo just
//! for ~250 LoC of dev-only helpers.

// Per-binary subset usage — see `common/mod.rs`.
#![allow(dead_code)]

use std::ops::Deref;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream::Config as StreamConfig;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

// ── URL helpers ─────────────────────────────────────────────────────────────

/// Default Postgres URL for test infrastructure.
/// Override with `TEST_POSTGRES_URL` env var.
pub const DEFAULT_POSTGRES_URL: &str = "postgres://mekhan:mekhan@localhost:15439/mekhan";

/// Default NATS URL for test infrastructure.
///
/// Points at the `just dev` stack broker (`docker-compose.yml` maps
/// `4333:4222`), which is the same NATS the engine/executor daemons connect
/// to. Override with `TEST_NATS_URL` env var.
pub const DEFAULT_NATS_URL: &str = "nats://localhost:4333";

/// Read the test Postgres URL from env or use the default.
pub fn postgres_url() -> String {
    std::env::var("TEST_POSTGRES_URL").unwrap_or_else(|_| DEFAULT_POSTGRES_URL.to_string())
}

/// Read the test NATS URL from env or use the default.
pub fn nats_url() -> String {
    std::env::var("TEST_NATS_URL").unwrap_or_else(|_| DEFAULT_NATS_URL.to_string())
}

// ── TestDb ──────────────────────────────────────────────────────────────────

/// An isolated test database with automatic cleanup.
///
/// Each instance creates a fresh database on the shared Postgres server.
/// The database is dropped when this struct is dropped (best-effort).
pub struct TestDb {
    pub pool: PgPool,
    pub db_name: String,
    admin_url: String,
}

impl TestDb {
    /// Create a fresh test database and run migrations.
    pub async fn create(migrations_path: &str) -> Self {
        let admin_url = postgres_url();
        let db_name = format!("test_{}", Uuid::new_v4().simple());

        let admin_pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&admin_url)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "Failed to connect to Postgres at {admin_url}: {e}\n\
                     Run: just dev::up (and the test infra stack)"
                )
            });

        admin_pool
            .execute(format!("CREATE DATABASE \"{}\"", db_name).as_str())
            .await
            .unwrap_or_else(|e| panic!("Failed to create test database {db_name}: {e}"));

        admin_pool.close().await;

        let test_url = replace_db_name(&admin_url, &db_name);

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&test_url)
            .await
            .unwrap_or_else(|e| panic!("Failed to connect to test database {db_name}: {e}"));

        sqlx::migrate::Migrator::new(std::path::Path::new(migrations_path))
            .await
            .unwrap_or_else(|e| panic!("Failed to load migrations from {migrations_path}: {e}"))
            .run(&pool)
            .await
            .unwrap_or_else(|e| panic!("Failed to run migrations on {db_name}: {e}"));

        tracing::debug!(db_name, "Test database created and migrated");

        Self {
            pool,
            db_name,
            admin_url,
        }
    }

    /// Explicitly drop the test database. Called automatically on Drop.
    pub async fn cleanup(&self) {
        self.pool.close().await;

        if let Ok(admin_pool) = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect(&self.admin_url)
            .await
        {
            let _ = admin_pool
                .execute(
                    format!(
                        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}'",
                        self.db_name
                    )
                    .as_str(),
                )
                .await;

            let _ = admin_pool
                .execute(format!("DROP DATABASE IF EXISTS \"{}\"", self.db_name).as_str())
                .await;

            admin_pool.close().await;
            tracing::debug!(db_name = self.db_name, "Test database dropped");
        }
    }
}

impl Deref for TestDb {
    type Target = PgPool;
    fn deref(&self) -> &PgPool {
        &self.pool
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin_url = self.admin_url.clone();
        let db_name = self.db_name.clone();

        let _ = std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(async {
                    if let Ok(pool) = PgPoolOptions::new()
                        .max_connections(1)
                        .acquire_timeout(Duration::from_secs(2))
                        .connect(&admin_url)
                        .await
                    {
                        let _ = pool
                            .execute(
                                format!(
                                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{}'",
                                    db_name
                                )
                                .as_str(),
                            )
                            .await;
                        let _ = pool
                            .execute(format!("DROP DATABASE IF EXISTS \"{}\"", db_name).as_str())
                            .await;
                        pool.close().await;
                    }
                });
            }
        });
    }
}

fn replace_db_name(url: &str, new_db: &str) -> String {
    if let Some(idx) = url.rfind('/') {
        format!("{}/{}", &url[..idx], new_db)
    } else {
        format!("{}/{}", url, new_db)
    }
}

// ── TestNats ────────────────────────────────────────────────────────────────

/// An isolated NATS JetStream context for a single test.
///
/// All streams and subjects are prefixed with a unique UUID to prevent
/// cross-test interference when running in parallel.
pub struct TestNats {
    pub client: async_nats::Client,
    pub jetstream: jetstream::Context,
    pub prefix: String,
    url: String,
    streams: Vec<String>,
}

impl TestNats {
    /// Connect to NATS and create an isolated test context.
    pub async fn connect() -> Self {
        let url = nats_url();
        Self::connect_to(&url).await
    }

    /// Connect to a specific NATS URL.
    pub async fn connect_to(url: &str) -> Self {
        let client = async_nats::connect(url).await.unwrap_or_else(|e| {
            panic!(
                "Failed to connect to NATS at {url}: {e}\n\
                 Run: just dev::up (and the test infra stack)"
            )
        });
        let jetstream = jetstream::new(client.clone());
        let prefix = format!("test_{}", Uuid::new_v4().simple());

        tracing::debug!(prefix, url, "TestNats context created");

        Self {
            client,
            jetstream,
            prefix,
            url: url.to_string(),
            streams: Vec::new(),
        }
    }

    /// Create an isolated stream with the test prefix.
    pub async fn create_stream(
        &mut self,
        base_name: &str,
        subjects: &[&str],
    ) -> jetstream::stream::Stream {
        let stream_name = format!("{}_{}", self.prefix, base_name);
        let prefixed_subjects: Vec<String> = subjects
            .iter()
            .map(|s| format!("{}.{}", self.prefix, s))
            .collect();

        let stream = self
            .jetstream
            .get_or_create_stream(StreamConfig {
                name: stream_name.clone(),
                subjects: prefixed_subjects,
                max_age: Duration::from_secs(300),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("Failed to create stream {stream_name}: {e}"));

        self.streams.push(stream_name.clone());
        tracing::debug!(stream_name, "Test stream created");

        stream
    }

    /// Get a prefixed subject for publishing/subscribing.
    pub fn subject(&self, subject: &str) -> String {
        format!("{}.{}", self.prefix, subject)
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Explicitly clean up all test streams.
    pub async fn cleanup(&self) {
        for stream_name in &self.streams {
            let _ = self.jetstream.delete_stream(stream_name).await;
        }
        tracing::debug!(prefix = self.prefix, "Test streams cleaned up");
    }
}

impl Drop for TestNats {
    fn drop(&mut self) {
        let jetstream = self.jetstream.clone();
        let streams = std::mem::take(&mut self.streams);
        let prefix = self.prefix.clone();

        let _ = std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(async {
                    for stream_name in &streams {
                        let _ = jetstream.delete_stream(stream_name).await;
                    }
                    tracing::debug!(prefix, "Test streams cleaned up (drop)");
                });
            }
        });
    }
}

// ── Wait helpers ────────────────────────────────────────────────────────────

/// Wait for Postgres to accept connections.
pub async fn wait_for_postgres(url: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(200);

    loop {
        match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(1))
            .connect(url)
            .await
        {
            Ok(pool) => {
                pool.close().await;
                tracing::info!("Postgres ready at {url}");
                return;
            }
            Err(_) if start.elapsed() < timeout => {
                tokio::time::sleep(poll_interval).await;
            }
            Err(e) => {
                panic!(
                    "Postgres not available at {url} after {:.1}s: {e}",
                    timeout.as_secs_f64()
                );
            }
        }
    }
}

/// Wait for NATS to accept connections.
pub async fn wait_for_nats(url: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(200);

    loop {
        match tokio::time::timeout(Duration::from_secs(1), async_nats::connect(url)).await {
            Ok(Ok(_client)) => {
                tracing::info!("NATS ready at {url}");
                return;
            }
            _ if start.elapsed() < timeout => {
                tokio::time::sleep(poll_interval).await;
            }
            _ => {
                panic!(
                    "NATS not available at {url} after {:.1}s",
                    timeout.as_secs_f64()
                );
            }
        }
    }
}
