use std::time::Duration;

use testcontainers::core::WaitFor;
use testcontainers::runners::AsyncRunner;
use testcontainers::GenericImage;
use tokio::sync::OnceCell;

struct SharedOllama {
    base_url: String,
    // `None` when `TEST_OLLAMA_URL` points us at an externally-managed daemon
    // (e.g. the native `ollama serve` that `just dev up-ollama` runs on
    // :11434); `Some` when we spun up our own hermetic testcontainer.
    _container: Option<testcontainers::ContainerAsync<GenericImage>>,
}

static SHARED_OLLAMA: OnceCell<SharedOllama> = OnceCell::const_new();

const OLLAMA_MODEL: &str = "qwen2.5:3b";

/// Env var pointing the suite at an already-running Ollama (e.g.
/// `http://localhost:11434`, the native daemon `just dev up-ollama` manages).
/// When set we skip the testcontainer entirely and just ensure the test model
/// is pulled. The container path only makes sense on Linux hosts where Docker
/// runs Ollama natively; on Apple Silicon the containerized Linux/CPU model
/// loader crashes at inference time (see `conformance.rs` `skip_reason`), so a
/// dev machine should export this to run against its native Metal daemon.
pub(crate) const OLLAMA_URL_ENV: &str = "TEST_OLLAMA_URL";

async fn shared_ollama() -> &'static SharedOllama {
    SHARED_OLLAMA
        .get_or_init(|| async {
            // Prefer an externally-managed daemon when pointed at one; only
            // spin up our own container as the hermetic (bare-CI) fallback.
            let (base_url, container) = match std::env::var(OLLAMA_URL_ENV) {
                Ok(url) if !url.trim().is_empty() => {
                    let url = url.trim().trim_end_matches('/').to_string();
                    eprintln!("Using externally-managed Ollama at {url} (via {OLLAMA_URL_ENV})");
                    (url, None)
                }
                _ => {
                    let container = GenericImage::new("ollama/ollama", "latest")
                        .with_exposed_port(11434.into())
                        .with_wait_for(WaitFor::message_on_stderr("Listening on"))
                        .start()
                        .await
                        .expect("Failed to start Ollama testcontainer");

                    let host = container.get_host().await.expect("get_host");
                    let port = container.get_host_port_ipv4(11434).await.expect("get_port");
                    let base_url = format!("http://{host}:{port}");
                    (base_url, Some(container))
                }
            };

            // Wait for API to be ready
            let client = reqwest::Client::new();
            for _ in 0..60 {
                match client.get(format!("{base_url}/api/tags")).send().await {
                    Ok(resp) if resp.status().is_success() => break,
                    _ => tokio::time::sleep(Duration::from_millis(500)).await,
                }
            }

            // Pull the model (idempotent — Ollama no-ops if the tag is already
            // present; the ~/.ollama blob store is shared with the dev daemon).
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
