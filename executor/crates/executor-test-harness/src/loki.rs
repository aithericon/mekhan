use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::GenericImage;
use tokio::sync::OnceCell;

struct SharedLoki {
    base_url: String,
    _container: testcontainers::ContainerAsync<GenericImage>,
}

static SHARED_LOKI: OnceCell<SharedLoki> = OnceCell::const_new();

async fn shared_loki() -> &'static SharedLoki {
    SHARED_LOKI
        .get_or_init(|| async {
            let container = GenericImage::new("grafana/loki", "3.3.2")
                .with_exposed_port(3100.into())
                .with_wait_for(WaitFor::message_on_stderr("Loki started"))
                .start()
                .await
                .expect("Failed to start Loki testcontainer");

            let host = container.get_host().await.expect("get_host");
            let port = container.get_host_port_ipv4(3100).await.expect("get_port");
            let base_url = format!("http://{host}:{port}");

            // Wait for /ready endpoint
            let client = reqwest::Client::new();
            for _ in 0..60 {
                match client.get(format!("{base_url}/ready")).send().await {
                    Ok(resp) if resp.status().is_success() => break,
                    _ => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
                }
            }

            SharedLoki {
                base_url,
                _container: container,
            }
        })
        .await
}

/// Returns the base URL for the shared Loki testcontainer (e.g. `http://localhost:32768`).
pub async fn shared_loki_base_url() -> &'static str {
    &shared_loki().await.base_url
}

/// Returns the Loki push URL (`/loki/api/v1/push`).
pub async fn shared_loki_push_url() -> String {
    format!("{}/loki/api/v1/push", shared_loki().await.base_url)
}

/// Returns the Loki query URL (`/loki/api/v1/query_range`).
pub async fn shared_loki_query_url() -> String {
    format!("{}/loki/api/v1/query_range", shared_loki().await.base_url)
}

/// Force Loki to flush its in-memory chunks to storage so they become queryable.
pub async fn flush_loki() {
    let base = shared_loki_base_url().await;
    let client = reqwest::Client::new();
    let _ = client.post(format!("{base}/flush")).send().await;
    // Give Loki a moment to finish flushing
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

/// Query Loki with a LogQL query over the last `since_secs` seconds.
/// Returns the parsed JSON response body.
pub async fn query_loki(logql: &str, since_secs: u64) -> serde_json::Value {
    let url = shared_loki_query_url().await;
    let now = chrono::Utc::now();
    let start = now - chrono::Duration::seconds(since_secs as i64);

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .query(&[
            ("query", logql),
            ("start", &start.to_rfc3339()),
            ("end", &now.to_rfc3339()),
            ("limit", "1000"),
        ])
        .send()
        .await
        .expect("Loki query request failed");

    resp.json::<serde_json::Value>()
        .await
        .expect("Loki query response not valid JSON")
}
