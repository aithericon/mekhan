use apalis::prelude::*;
use apalis_nats::{Config, NatsStorage, Priority};
use async_nats::jetstream::{self, consumer};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::nats::Nats;
use tokio::sync::Mutex;
use uuid::Uuid;

// Test job structure
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestJob {
    id: String,
    message: String,
    attempt: usize,
}

impl TestJob {
    fn new(message: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            message: message.into(),
            attempt: 0,
        }
    }
}

// Helper to setup NATS container and storage
async fn setup_nats() -> (ContainerAsync<Nats>, NatsStorage<TestJob>) {
    // Start NATS container with JetStream enabled
    let container = Nats::default()
        .with_cmd(["-js"]) // Enable JetStream
        .start()
        .await
        .expect("Failed to start NATS container");

    let host = container.get_host().await.expect("Failed to get host");
    let port = container
        .get_host_port_ipv4(4222)
        .await
        .expect("Failed to get port");

    let nats_url = format!("nats://{}:{}", host, port);

    // Give NATS a moment to fully initialize
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Connect to NATS
    let client = apalis_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    // Create storage with test configuration
    let config = Config {
        namespace: format!("test_{}", Uuid::new_v4().to_string().replace('-', "_")),
        max_deliver: 3,
        ack_wait: Duration::from_secs(5),
        num_replicas: 1,
        enable_dlq: true,
        max_ack_pending: 10, // Lower for testing to avoid message duplication
        ..Default::default()
    };

    let storage = NatsStorage::new_with_config(client, config)
        .await
        .expect("Failed to create NATS storage");

    (container, storage)
}

// Helper to setup NATS container and return raw client
async fn setup_nats_raw() -> (ContainerAsync<Nats>, async_nats::Client) {
    // Start NATS container with JetStream enabled
    let container = Nats::default()
        .with_cmd(["-js"]) // Enable JetStream
        .start()
        .await
        .expect("Failed to start NATS container");

    let host = container.get_host().await.expect("Failed to get host");
    let port = container
        .get_host_port_ipv4(4222)
        .await
        .expect("Failed to get port");

    let nats_url = format!("nats://{}:{}", host, port);

    // Give NATS a moment to fully initialize
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Connect to NATS
    let client = apalis_nats::connect(&nats_url)
        .await
        .expect("Failed to connect to NATS");

    (container, client)
}

#[tokio::test]
async fn test_end_to_end_job_execution() {
    // Initialize tracing for debugging
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis=debug,apalis_nats=debug")
        .try_init();

    let (_container, mut storage) = setup_nats().await;

    // Track job execution
    let executed_jobs = Arc::new(Mutex::new(Vec::<TestJob>::new()));
    let executed_clone = executed_jobs.clone();

    // Job handler
    async fn handle_job(
        job: TestJob,
        executed: Data<Arc<Mutex<Vec<TestJob>>>>,
    ) -> Result<(), Error> {
        println!("Processing job: {:?}", job);
        executed.lock().await.push(job);
        Ok(())
    }

    // Push test jobs
    let jobs = vec![
        TestJob::new("Job 1"),
        TestJob::new("Job 2"),
        TestJob::new("Job 3"),
    ];

    let mut pushed_jobs = Vec::new();
    for job in &jobs {
        let task_id = storage.push(job.clone()).await.expect("Failed to push job");
        println!("Pushed job {} with ID: {:?}", job.id, &task_id);
        pushed_jobs.push(task_id);
    }

    // Create and run worker
    let worker = WorkerBuilder::new("test-worker")
        .concurrency(2)
        .data(executed_clone)
        .backend(storage.clone())
        .build_fn(handle_job);

    // Run worker for a limited time
    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Wait for jobs to be processed
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check results
    let executed = executed_jobs.lock().await;
    assert_eq!(executed.len(), 3, "All jobs should be executed");

    // Verify all jobs were executed
    for job in &jobs {
        assert!(
            executed.iter().any(|e| e.id == job.id),
            "Job {} should be executed",
            job.id
        );
    }

    // Cleanup
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn test_priority_queue_ordering() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis=debug,apalis_nats=debug")
        .try_init();

    let (_container, storage) = setup_nats().await;

    // Track execution order
    let execution_order = Arc::new(Mutex::new(Vec::<String>::new()));
    let order_clone = execution_order.clone();

    async fn track_job(job: TestJob, order: Data<Arc<Mutex<Vec<String>>>>) -> Result<(), Error> {
        println!("Processing job with message: {}", job.message);
        order.lock().await.push(job.message.clone());
        // Simulate some work
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    // Push jobs with different priorities
    // Push low priority first, then medium, then high
    for i in 1..=3 {
        let job = TestJob::new(format!("Low priority {}", i));
        storage
            .push_with_priority(job, Priority::Low)
            .await
            .expect("Failed to push low priority job");
    }

    for i in 1..=3 {
        let job = TestJob::new(format!("Medium priority {}", i));
        storage
            .push_with_priority(job, Priority::Medium)
            .await
            .expect("Failed to push medium priority job");
    }

    for i in 1..=3 {
        let job = TestJob::new(format!("High priority {}", i));
        storage
            .push_with_priority(job, Priority::High)
            .await
            .expect("Failed to push high priority job");
    }

    // Give jobs time to be persisted
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Create worker with single concurrency to ensure sequential processing
    let worker = WorkerBuilder::new("priority-test-worker")
        .concurrency(1)
        .data(order_clone)
        .backend(storage.clone())
        .build_fn(track_job);

    // Run worker
    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Wait for processing
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Check execution order
    let order = execution_order.lock().await;
    println!("Execution order: {:?}", *order);

    // High priority jobs should be processed first
    assert!(
        order[0].starts_with("High priority"),
        "First job should be high priority, got: {}",
        order[0]
    );
    assert!(
        order[1].starts_with("High priority"),
        "Second job should be high priority, got: {}",
        order[1]
    );
    assert!(
        order[2].starts_with("High priority"),
        "Third job should be high priority, got: {}",
        order[2]
    );

    // Then medium priority
    assert!(
        order[3].starts_with("Medium priority"),
        "Fourth job should be medium priority, got: {}",
        order[3]
    );

    // Cleanup
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn test_job_retry_and_failure() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis=debug,apalis_nats=debug")
        .try_init();

    let (_container, mut storage) = setup_nats().await;

    // Track retry attempts
    let attempt_counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = attempt_counter.clone();

    async fn failing_job(job: TestJob, counter: Data<Arc<AtomicUsize>>) -> Result<(), Error> {
        let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
        println!("Job {} attempt #{}", job.id, attempt);

        if attempt < 3 {
            // Fail the first 2 attempts
            println!("Failing job {} on attempt {}", job.id, attempt);
            Err(Error::Failed(Arc::new(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Intentional failure on attempt {}", attempt),
            ))
                as Box<dyn std::error::Error + Send + Sync>)))
        } else {
            // Succeed on the third attempt
            println!("Job {} succeeded on attempt {}", job.id, attempt);
            Ok(())
        }
    }

    // Push a job that will fail initially
    let job = TestJob::new("Retry test job");
    let _job_id = job.id.clone();
    storage.push(job).await.expect("Failed to push job");

    // Create worker
    let worker = WorkerBuilder::new("retry-test-worker")
        .concurrency(1)
        .data(counter_clone)
        .backend(storage.clone())
        .build_fn(failing_job);

    // Run worker
    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Wait for retries to complete
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Check that job was retried
    let attempts = attempt_counter.load(Ordering::SeqCst);
    assert!(
        attempts >= 3,
        "Job should have been attempted at least 3 times, got {} attempts",
        attempts
    );

    // Cleanup
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn test_concurrent_workers() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis=debug,apalis_nats=debug")
        .try_init();

    let (_container, mut storage) = setup_nats().await;

    // Track which worker processed each job
    let processed_jobs = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let processed_clone1 = processed_jobs.clone();
    let processed_clone2 = processed_jobs.clone();

    async fn process_job(
        job: TestJob,
        worker_id: Data<String>,
        processed: Data<Arc<Mutex<Vec<(String, String)>>>>,
    ) -> Result<(), Error> {
        println!("Worker {} processing job {}", *worker_id, job.id);
        // Simulate work
        tokio::time::sleep(Duration::from_millis(500)).await;
        processed
            .lock()
            .await
            .push((job.id.clone(), worker_id.to_string()));
        Ok(())
    }

    // Push multiple jobs
    let num_jobs = 10;
    for i in 0..num_jobs {
        let job = TestJob::new(format!("Concurrent job {}", i));
        storage.push(job).await.expect("Failed to push job");
    }

    // Create two workers
    let worker1 = WorkerBuilder::new("worker-1")
        .concurrency(2)
        .data("worker-1".to_string())
        .data(processed_clone1)
        .backend(storage.clone())
        .build_fn(process_job);

    let worker2 = WorkerBuilder::new("worker-2")
        .concurrency(2)
        .data("worker-2".to_string())
        .data(processed_clone2)
        .backend(storage.clone())
        .build_fn(process_job);

    // Run both workers
    let handle1 = tokio::spawn(async move {
        worker1.run().await;
    });

    let handle2 = tokio::spawn(async move {
        worker2.run().await;
    });

    // Wait for processing
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Check results
    let processed = processed_jobs.lock().await;
    assert_eq!(processed.len(), num_jobs, "All jobs should be processed");

    // Verify that both workers processed some jobs
    let worker1_count = processed.iter().filter(|(_, w)| w == "worker-1").count();
    let worker2_count = processed.iter().filter(|(_, w)| w == "worker-2").count();

    println!("Worker 1 processed {} jobs", worker1_count);
    println!("Worker 2 processed {} jobs", worker2_count);

    assert!(worker1_count > 0, "Worker 1 should process some jobs");
    assert!(worker2_count > 0, "Worker 2 should process some jobs");
    assert_eq!(
        worker1_count + worker2_count,
        num_jobs,
        "All jobs should be accounted for"
    );

    // Cleanup
    handle1.abort();
    handle2.abort();
    let _ = handle1.await;
    let _ = handle2.await;
}

#[tokio::test]
async fn test_storage_stats() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis=debug,apalis_nats=debug")
        .try_init();

    let (_container, mut storage) = setup_nats().await;

    // Initially, storage should be empty
    let initial_count = storage.len().await.expect("Failed to get length");
    assert_eq!(initial_count, 0, "Storage should initially be empty");

    // Push some jobs
    let num_jobs = 5;
    for i in 0..num_jobs {
        let job = TestJob::new(format!("Stats test job {}", i));
        storage.push(job).await.expect("Failed to push job");
    }

    // Check count after pushing
    let count = storage.len().await.expect("Failed to get length");
    assert_eq!(count, num_jobs, "Storage should contain {} jobs", num_jobs);

    // Check if empty
    let is_empty = storage.is_empty().await.expect("Failed to check if empty");
    assert!(!is_empty, "Storage should not be empty");

    // Process jobs
    async fn consume_job(_job: TestJob) -> Result<(), Error> {
        Ok(())
    }

    let worker = WorkerBuilder::new("stats-test-worker")
        .concurrency(2)
        .backend(storage.clone())
        .build_fn(consume_job);

    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Wait for processing
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check count after processing
    let final_count = storage.len().await.expect("Failed to get length");
    assert_eq!(
        final_count, 0,
        "Storage should be empty after processing all jobs"
    );

    // Cleanup
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn test_dlq_on_max_deliveries() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis=debug,apalis_nats=debug")
        .try_init();

    let (_container, client) = setup_nats_raw().await;

    // Create storage with max_deliver = 2 for faster testing
    let config = Config {
        namespace: format!(
            "test_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "_")
        ),
        max_deliver: 2, // Only 2 attempts before DLQ
        enable_dlq: true,
        ..Default::default()
    };

    let mut storage = NatsStorage::<TestJob>::new_with_config(client.clone(), config.clone())
        .await
        .expect("Failed to create storage");

    // Track attempt count
    let attempt_counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = attempt_counter.clone();

    async fn always_failing_job(
        job: TestJob,
        counter: Data<Arc<AtomicUsize>>,
    ) -> Result<(), Error> {
        let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
        println!("Job {} attempt #{} (will always fail)", job.id, attempt);

        // Always fail with a transient error
        Err(Error::Failed(Arc::new(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Intentional failure on attempt {}", attempt),
        ))
            as Box<dyn std::error::Error + Send + Sync>)))
    }

    // Push a job that will always fail
    let job = TestJob::new("DLQ test job");
    let _job_id = job.id.clone();
    storage.push(job).await.expect("Failed to push job");

    // Create worker
    let worker = WorkerBuilder::new("dlq-test-worker")
        .concurrency(1)
        .data(counter_clone)
        .backend(storage.clone())
        .build_fn(always_failing_job);

    // Run worker
    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Wait for max delivery attempts
    tokio::time::sleep(Duration::from_secs(8)).await;

    // Verify job was attempted exactly max_deliver times
    let attempts = attempt_counter.load(Ordering::SeqCst);
    assert_eq!(
        attempts, 2,
        "Job should have been attempted exactly {} times before moving to DLQ, got {} attempts",
        config.max_deliver, attempts
    );

    // Check DLQ stream for the failed job
    let jetstream = jetstream::new(client);
    let dlq_stream_name = format!("{}_dlq", config.namespace);

    if let Ok(mut stream) = jetstream.get_stream(dlq_stream_name.clone()).await {
        let info = stream.info().await.expect("Failed to get stream info");
        assert!(
            info.state.messages > 0,
            "DLQ should contain the failed job, but has {} messages",
            info.state.messages
        );
        println!("DLQ contains {} message(s)", info.state.messages);

        // Try to read the DLQ message to verify its content
        let _dlq_subject = format!("{}.dlq", config.namespace);
        if let Ok(consumer) = stream
            .create_consumer(consumer::pull::Config {
                durable_name: Some("dlq-reader".to_string()),
                ..Default::default()
            })
            .await
        {
            if let Ok(mut messages) = consumer.messages().await {
                if let Ok(Some(msg)) = messages.try_next().await {
                    let dlq_data: serde_json::Value =
                        serde_json::from_slice(&msg.payload).expect("Failed to parse DLQ message");

                    // Verify DLQ message contains expected fields
                    assert!(
                        dlq_data.get("original_task_id").is_some(),
                        "DLQ message should contain original_task_id"
                    );
                    assert!(
                        dlq_data.get("error").is_some(),
                        "DLQ message should contain error"
                    );
                    assert!(
                        dlq_data.get("delivered_count").is_some(),
                        "DLQ message should contain delivered_count"
                    );
                    assert_eq!(
                        dlq_data.get("delivered_count").and_then(|v| v.as_u64()),
                        Some(2),
                        "Delivered count should be 2"
                    );

                    println!("DLQ message verified: {:?}", dlq_data);

                    // Acknowledge to clean up
                    msg.ack().await.ok();
                }
            }
        }
    } else {
        panic!(
            "DLQ stream {} should exist but was not found",
            dlq_stream_name
        );
    }

    // Cleanup
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn test_dlq_on_abort_error() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis=debug,apalis_nats=debug")
        .try_init();

    let (_container, client) = setup_nats_raw().await;

    // Create storage with DLQ enabled
    let config = Config {
        namespace: format!(
            "test_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "_")
        ),
        enable_dlq: true,
        ..Default::default()
    };

    let mut storage = NatsStorage::<TestJob>::new_with_config(client.clone(), config.clone())
        .await
        .expect("Failed to create storage");

    async fn abort_job(job: TestJob) -> Result<(), Error> {
        println!("Job {} will abort immediately", job.id);
        // Return Error::Abort which should immediately send to DLQ
        Err(Error::Abort(Arc::new(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Non-transient error - send to DLQ",
        ))
            as Box<dyn std::error::Error + Send + Sync>)))
    }

    // Push a job that will abort
    let job = TestJob::new("Abort test job");
    let _job_id = job.id.clone();
    storage.push(job).await.expect("Failed to push job");

    // Create worker
    let worker = WorkerBuilder::new("abort-test-worker")
        .concurrency(1)
        .backend(storage.clone())
        .build_fn(abort_job);

    // Run worker
    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Wait for job to be processed
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check DLQ stream for the aborted job
    let jetstream = jetstream::new(client);
    let dlq_stream_name = format!("{}_dlq", config.namespace);

    if let Ok(mut stream) = jetstream.get_stream(dlq_stream_name.clone()).await {
        let info = stream.info().await.expect("Failed to get stream info");
        assert!(
            info.state.messages > 0,
            "DLQ should contain the aborted job, but has {} messages",
            info.state.messages
        );
        println!(
            "DLQ contains {} message(s) after abort",
            info.state.messages
        );
    } else {
        panic!(
            "DLQ stream {} should exist but was not found",
            dlq_stream_name
        );
    }

    // Cleanup
    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn test_long_running_with_heartbeat_prevents_redelivery() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("apalis_nats=debug,apalis=info")
        .try_init();

    let (_container, client) = setup_nats_raw().await;

    // Small ack_wait to force redelivery quickly if no progress
    let config = Config {
        namespace: format!(
            "test_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "_")
        ),
        ack_wait: Duration::from_secs(2),
        max_deliver: 2,
        enable_dlq: true,
        ..Default::default()
    };

    let mut storage = NatsStorage::<TestJob>::new_with_config(client.clone(), config.clone())
        .await
        .expect("Failed to create storage");

    // Record observed delivered counts during processing
    let observed = Arc::new(Mutex::new(Vec::<u64>::new()));
    let observed_clone = observed.clone();

    async fn long_job_with_heartbeat(
        _job: TestJob,
        ctx: apalis_nats::NatsContext,
        observed: Data<Arc<Mutex<Vec<u64>>>>,
    ) -> Result<(), Error> {
        // Start heartbeat every 1s (< ack_wait)
        let _hb = ctx.start_progress_heartbeat(Duration::from_secs(1));
        // Run for ~5 seconds, sampling delivered count
        for _ in 0..5 {
            if let Some(msg) = ctx.message() {
                if let Ok(info) = msg.info() {
                    observed
                        .lock()
                        .await
                        .push(u64::try_from(info.delivered).unwrap_or(u64::MAX));
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        // Finish successfully
        Ok(())
    }

    // Push a job that would run longer than ack_wait
    let job = TestJob::new("Long-running with heartbeat");
    storage.push(job).await.expect("Failed to push job");

    let worker = WorkerBuilder::new("heartbeat-long-job")
        .concurrency(1)
        .data(observed_clone)
        .backend(storage.clone())
        .build_fn(long_job_with_heartbeat);

    // Run worker
    let handle = tokio::spawn(async move {
        worker.run().await;
    });

    // Wait long enough for the handler to finish
    tokio::time::sleep(Duration::from_secs(7)).await;

    // Check that delivered was never observed > 1 during processing
    let obs = observed.lock().await.clone();
    assert!(
        !obs.iter().any(|&d| d > 1),
        "Observed redelivery despite heartbeats: {:?}",
        obs
    );

    // Ensure DLQ is empty for this namespace
    let js = jetstream::new(client);
    let dlq_stream_name = format!("{}_dlq", config.namespace);
    if let Ok(mut stream) = js.get_stream(dlq_stream_name.clone()).await {
        let info = stream.info().await.expect("failed to fetch dlq info");
        assert_eq!(
            info.state.messages, 0,
            "No DLQ messages expected when heartbeat is active"
        );
    }

    handle.abort();
    let _ = handle.await;
}
