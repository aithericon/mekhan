use std::time::Duration;

use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::GenericImage;
use tokio::sync::OnceCell;

struct SharedOllama {
    base_url: String,
    _container: testcontainers::ContainerAsync<GenericImage>,
}

static SHARED_OLLAMA: OnceCell<SharedOllama> = OnceCell::const_new();

const OLLAMA_MODEL: &str = "qwen2.5:3b";

async fn shared_ollama() -> &'static SharedOllama {
    SHARED_OLLAMA
        .get_or_init(|| async {
            let container = GenericImage::new("ollama/ollama", "latest")
                .with_exposed_port(11434.into())
                .with_wait_for(WaitFor::message_on_stderr("Listening on"))
                .start()
                .await
                .expect("Failed to start Ollama testcontainer");

            let host = container.get_host().await.expect("get_host");
            let port = container.get_host_port_ipv4(11434).await.expect("get_port");
            let base_url = format!("http://{host}:{port}");

            // Wait for API to be ready
            let client = reqwest::Client::new();
            for _ in 0..60 {
                match client.get(format!("{base_url}/api/tags")).send().await {
                    Ok(resp) if resp.status().is_success() => break,
                    _ => tokio::time::sleep(Duration::from_millis(500)).await,
                }
            }

            // Pull the model (blocks until download completes)
            eprintln!(
                "Pulling Ollama model {OLLAMA_MODEL} (this may take a few minutes on first run)..."
            );
            let pull_resp = client
                .post(format!("{base_url}/api/pull"))
                .json(&serde_json::json!({
                    "name": OLLAMA_MODEL,
                    "stream": false,
                }))
                .timeout(Duration::from_secs(600))
                .send()
                .await
                .expect("model pull request failed");

            assert!(
                pull_resp.status().is_success(),
                "model pull failed: {}",
                pull_resp.text().await.unwrap_or_default()
            );
            eprintln!("Model {OLLAMA_MODEL} ready.");

            SharedOllama {
                base_url,
                _container: container,
            }
        })
        .await
}

/// Returns the base URL for the shared Ollama testcontainer (e.g. `http://localhost:32768`).
pub async fn shared_ollama_base_url() -> &'static str {
    &shared_ollama().await.base_url
}

/// Returns the model name available in the shared Ollama testcontainer.
pub fn ollama_model() -> &'static str {
    OLLAMA_MODEL
}
