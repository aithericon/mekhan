//! Official model-catalog proxy (the operator's model browser).
//!
//! `GET /api/v1/model-catalog/{source}?q=` fetches a discovery list from the
//! upstream OFFICIAL catalog for a backend so the operator can browse + provision
//! without memorising exact ids:
//!
//! - **`ollama`** — scrapes `ollama.com/search?q=` (the Ollama library has no
//!   documented JSON API). Parses the page's stable `x-test-*` hooks into a model
//!   list. Each entry's `id` is the library slug (`llama3.2`); the engine card's
//!   "Provision" then issues a `Pull` `ModelCommand` to a runner, which runs
//!   `ollama pull` to fetch the weights.
//! - **`huggingface`** — calls HF's public `GET /api/models` JSON API (the vLLM
//!   source). Informational on a vLLM node — its base is fixed at engine launch,
//!   so an HF id is copied into config / a dedicated job rather than hot-loaded —
//!   but an Ollama node CAN pull a GGUF repo (`hf.co/<id>`).
//!
//! This is mekhan reaching OUT to public catalogs for METADATA only (model names,
//! sizes, pull counts). It is NOT inference and carries no workspace data, so it
//! is orthogonal to the GDPR no-auto-offload invariant (that governs where
//! inference runs, not where the operator browses). Results are cached in-process
//! (~10 min TTL) so a browsing operator doesn't hammer the upstreams.
//!
//! Fail-soft: an upstream error / parse miss returns an empty list with a
//! `stale`/`error` hint rather than failing the operator's browse.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::auth::AuthUser;
use crate::models::error::ApiError;

/// One model in an upstream catalog browse result.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CatalogModel {
    /// The provision id — the exact string a `Pull`/`Load` command takes. Ollama
    /// library slug (`llama3.2`) or HF repo id (`meta-llama/Llama-3.2-1B`).
    pub id: String,
    /// Human display name (== `id` for HF; the title for Ollama).
    pub name: String,
    /// A one-line blurb when the upstream provides one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Popularity hint — Ollama's "5M" pull count, or HF's download integer as a
    /// string. Display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulls: Option<String>,
    /// Parameter-size tags the upstream advertises (Ollama `1b`/`3b`/…); empty for
    /// HF (which exposes no clean size facet here).
    #[serde(default)]
    pub sizes: Vec<String>,
    /// Capability tags (Ollama `tools`/`vision`/…; HF pipeline/library tags).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Which catalog this came from (`ollama` | `huggingface`).
    pub source: String,
    /// Link to the model's upstream page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `GET /api/v1/model-catalog/{source}` response.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelCatalogResponse {
    /// The catalog source echoed back.
    pub source: String,
    /// Browse results (possibly empty on an upstream error — see `error`).
    pub models: Vec<CatalogModel>,
    /// `true` when these results were served from the in-process cache.
    pub cached: bool,
    /// A fail-soft error hint when the upstream fetch/parse failed (results then
    /// empty or stale-cached). `None` on a clean fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `?q=` browse query.
#[derive(Debug, Deserialize, IntoParams)]
pub struct CatalogQuery {
    /// Free-text search; empty / absent ⇒ the upstream's popular/trending list.
    #[serde(default)]
    pub q: Option<String>,
}

// ── In-process TTL cache ─────────────────────────────────────────────────────

const CACHE_TTL: Duration = Duration::from_secs(600);

/// `(source, normalized query) → (fetched_at, models)`. A browsing operator polls
/// the same query repeatedly; caching keeps us off the upstreams' rate limits.
type CatalogCache = HashMap<String, (Instant, Vec<CatalogModel>)>;
static CACHE: LazyLock<Mutex<CatalogCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache_get(key: &str) -> Option<Vec<CatalogModel>> {
    let guard = CACHE.lock().ok()?;
    let (at, models) = guard.get(key)?;
    if at.elapsed() < CACHE_TTL {
        Some(models.clone())
    } else {
        None
    }
}

fn cache_put(key: String, models: &[CatalogModel]) {
    if let Ok(mut guard) = CACHE.lock() {
        guard.insert(key, (Instant::now(), models.to_vec()));
    }
}

/// Stale entry (any age) — last resort when a live fetch fails, so a transient
/// upstream blip still serves the operator something.
fn cache_get_stale(key: &str) -> Option<Vec<CatalogModel>> {
    let guard = CACHE.lock().ok()?;
    guard.get(key).map(|(_, m)| m.clone())
}

/// `GET /api/v1/model-catalog/{source}` — browse an upstream OFFICIAL catalog
/// (`ollama` scrape | `huggingface` JSON API). Session/human authed; metadata
/// only (no inference, no workspace data). Cached ~10 min; fail-soft to empty.
#[utoipa::path(
    get,
    path = "/api/v1/model-catalog/{source}",
    params(
        ("source" = String, Path, description = "Catalog source: `ollama` or `huggingface`"),
        CatalogQuery,
    ),
    responses(
        (status = 200, description = "Upstream model browse results", body = ModelCatalogResponse),
        (status = 400, description = "Unknown catalog source"),
    ),
    tag = "models",
)]
pub async fn browse_model_catalog(
    _user: AuthUser,
    Path(source): Path<String>,
    Query(query): Query<CatalogQuery>,
) -> Result<Json<ModelCatalogResponse>, ApiError> {
    let source = source.to_lowercase();
    if source != "ollama" && source != "huggingface" {
        return Err(ApiError::bad_request(format!(
            "unknown catalog source '{source}' (expected 'ollama' or 'huggingface')"
        )));
    }

    let q = query.q.unwrap_or_default();
    let q = q.trim();
    let cache_key = format!("{source}:{}", q.to_lowercase());

    if let Some(models) = cache_get(&cache_key) {
        return Ok(Json(ModelCatalogResponse {
            source,
            models,
            cached: true,
            error: None,
        }));
    }

    let fetched = match source.as_str() {
        "ollama" => fetch_ollama(q).await,
        _ => fetch_huggingface(q).await,
    };

    match fetched {
        Ok(models) => {
            cache_put(cache_key, &models);
            Ok(Json(ModelCatalogResponse {
                source,
                models,
                cached: false,
                error: None,
            }))
        }
        Err(e) => {
            // Fail-soft: serve a stale cache entry if we have one, else empty +
            // the error hint, so the operator's browse never hard-fails.
            let stale = cache_get_stale(&cache_key);
            Ok(Json(ModelCatalogResponse {
                source,
                models: stale.unwrap_or_default(),
                cached: false,
                error: Some(e),
            }))
        }
    }
}

/// A shared outbound client with a browse-friendly timeout + a UA (some upstreams
/// 403 a UA-less request).
fn outbound_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("mekhan-control-plane/model-browser")
        .build()
        .map_err(|e| format!("client build: {e}"))
}

// ── Ollama library (HTML scrape of ollama.com/search) ────────────────────────

/// Fetch + parse `ollama.com/search?q=` into a model list. The library has no
/// JSON API, so we parse the page's stable `x-test-*` test hooks: each model is a
/// block beginning at `x-test-model` carrying `/library/<slug>`, a title, sizes,
/// capabilities, and a pull count.
async fn fetch_ollama(q: &str) -> Result<Vec<CatalogModel>, String> {
    let client = outbound_client()?;
    let url = format!(
        "https://ollama.com/search?q={}",
        urlencoding_encode(q)
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("ollama.com fetch: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("ollama.com returned {}", resp.status()));
    }
    let html = resp
        .text()
        .await
        .map_err(|e| format!("ollama.com body: {e}"))?;
    Ok(parse_ollama_search(&html))
}

/// Pure parser (unit-testable without a network) — split the page into per-model
/// blocks on the `x-test-model` marker and pull the fields out of each.
fn parse_ollama_search(html: &str) -> Vec<CatalogModel> {
    use regex::Regex;
    static RE_SLUG: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"href="/library/([a-zA-Z0-9._-]+)""#).unwrap());
    static RE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"x-test-search-response-title[^>]*>\s*([^<]+?)\s*<"#).unwrap()
    });
    static RE_SIZE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"x-test-size[^>]*>\s*([^<]+?)\s*<"#).unwrap());
    static RE_CAP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"x-test-capability[^>]*>\s*([^<]+?)\s*<"#).unwrap());
    static RE_PULLS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"x-test-pull-count[^>]*>\s*([^<]+?)\s*<"#).unwrap());

    let mut out = Vec::new();
    // The first split chunk is the page header (no model); each subsequent chunk
    // is one model's markup up to the next marker.
    for chunk in html.split("x-test-model").skip(1) {
        let Some(slug) = RE_SLUG
            .captures(chunk)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
        else {
            continue;
        };
        let name = RE_TITLE
            .captures(chunk)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| slug.clone());
        let sizes: Vec<String> = RE_SIZE
            .captures_iter(chunk)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();
        let capabilities: Vec<String> = RE_CAP
            .captures_iter(chunk)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect();
        let pulls = RE_PULLS
            .captures(chunk)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string());

        out.push(CatalogModel {
            id: slug.clone(),
            name,
            description: None,
            pulls,
            sizes,
            capabilities,
            source: "ollama".into(),
            url: Some(format!("https://ollama.com/library/{slug}")),
        });
    }
    out
}

// ── Hugging Face Hub (public JSON API) ───────────────────────────────────────

#[derive(Debug, Deserialize)]
struct HfModel {
    id: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    pipeline_tag: Option<String>,
    #[serde(default)]
    library_name: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

/// Fetch HF's `GET /api/models` (text-generation, by downloads). Public JSON API,
/// so no scraping. `q` is the free-text search.
async fn fetch_huggingface(q: &str) -> Result<Vec<CatalogModel>, String> {
    let client = outbound_client()?;
    let url = format!(
        "https://huggingface.co/api/models?search={}&filter=text-generation&sort=downloads&direction=-1&limit=40",
        urlencoding_encode(q)
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("huggingface.co fetch: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("huggingface.co returned {}", resp.status()));
    }
    let models: Vec<HfModel> = resp
        .json()
        .await
        .map_err(|e| format!("huggingface.co parse: {e}"))?;
    Ok(models.into_iter().map(hf_to_catalog).collect())
}

fn hf_to_catalog(m: HfModel) -> CatalogModel {
    // Surface a few human tags (pipeline + library + a couple of facet tags) as
    // capabilities, skipping the noisy machine tags (`region:…`, `arxiv:…`, …).
    let mut capabilities: Vec<String> = Vec::new();
    if let Some(p) = &m.pipeline_tag {
        capabilities.push(p.clone());
    }
    if let Some(l) = &m.library_name {
        capabilities.push(l.clone());
    }
    capabilities.extend(
        m.tags
            .iter()
            .filter(|t| !t.contains(':') && t.len() < 24)
            .take(4)
            .cloned(),
    );
    capabilities.dedup();

    CatalogModel {
        id: m.id.clone(),
        name: m.id.clone(),
        description: m.pipeline_tag.clone(),
        pulls: Some(format_downloads(m.downloads)),
        sizes: Vec::new(),
        capabilities,
        source: "huggingface".into(),
        url: Some(format!("https://huggingface.co/{}", m.id)),
    }
}

/// Compact a download integer to a human "1.2M" / "34K" hint.
fn format_downloads(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Minimal percent-encoding for the `q` query value (no extra dep — only the
/// handful of chars a model search realistically contains).
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ollama_search_blocks() {
        // Two minimal model blocks in the page's `x-test-*` shape.
        let html = r##"
            <header>nope</header>
            <li x-test-model>
              <a href="/library/llama3.2"><span x-test-search-response-title>Llama 3.2</span></a>
              <span x-test-capability>tools</span>
              <span x-test-size>1b</span><span x-test-size>3b</span>
              <span x-test-pull-count>21M</span>
            </li>
            <li x-test-model>
              <a href="/library/qwen2.5"><span x-test-search-response-title>Qwen2.5</span></a>
              <span x-test-size>0.5b</span>
              <span x-test-pull-count>8M</span>
            </li>
        "##;
        let models = parse_ollama_search(html);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "llama3.2");
        assert_eq!(models[0].name, "Llama 3.2");
        assert_eq!(models[0].sizes, vec!["1b", "3b"]);
        assert_eq!(models[0].capabilities, vec!["tools"]);
        assert_eq!(models[0].pulls.as_deref(), Some("21M"));
        assert_eq!(
            models[0].url.as_deref(),
            Some("https://ollama.com/library/llama3.2")
        );
        assert_eq!(models[1].id, "qwen2.5");
        assert_eq!(models[1].sizes, vec!["0.5b"]);
    }

    #[test]
    fn ollama_block_without_slug_is_skipped() {
        let html = "x-test-model <li>no library link here</li>";
        assert!(parse_ollama_search(html).is_empty());
    }

    #[test]
    fn hf_maps_id_downloads_and_filters_machine_tags() {
        let m = HfModel {
            id: "meta-llama/Llama-3.2-1B-Instruct".into(),
            downloads: 8_292_089,
            pipeline_tag: Some("text-generation".into()),
            library_name: Some("transformers".into()),
            tags: vec![
                "safetensors".into(),
                "region:us".into(),
                "arxiv:2204.05149".into(),
                "conversational".into(),
            ],
        };
        let c = hf_to_catalog(m);
        assert_eq!(c.id, "meta-llama/Llama-3.2-1B-Instruct");
        assert_eq!(c.pulls.as_deref(), Some("8.3M"));
        assert!(c.capabilities.contains(&"text-generation".to_string()));
        assert!(c.capabilities.contains(&"transformers".to_string()));
        assert!(c.capabilities.contains(&"conversational".to_string()));
        // Machine tags filtered out.
        assert!(!c.capabilities.iter().any(|t| t.contains(':')));
    }

    #[test]
    fn format_downloads_compacts() {
        assert_eq!(format_downloads(950), "950");
        assert_eq!(format_downloads(34_000), "34K");
        assert_eq!(format_downloads(1_200_000), "1.2M");
    }

    #[test]
    fn urlencoding_handles_spaces_and_slashes() {
        assert_eq!(urlencoding_encode("llama 3"), "llama%203");
        assert_eq!(urlencoding_encode("meta/llama"), "meta%2Fllama");
    }
}
