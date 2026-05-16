use std::net::SocketAddr;
use std::sync::Arc;

use mekhan_service::auth::authenticator::{
    Authenticator, BffAuthenticator, NoopAuthenticator,
};
use mekhan_service::auth::bff::oidc::{OidcClient, OidcConfig};
use mekhan_service::auth::bff::session::{PgSessionStore, SessionStore};
use mekhan_service::auth::dev::NoopTokenVerifier;
use mekhan_service::auth::resolver::StaticPrincipalResolver;
use mekhan_service::auth::zitadel::{ZitadelConfig, ZitadelTokenVerifier};
use mekhan_service::auth::{PrincipalResolver, TokenVerifier};
use mekhan_service::config::{AppConfig, AuthMode};
use mekhan_service::db;
use mekhan_service::lifecycle;
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::s3::ArtifactStore;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
use mekhan_service::catalogue::repository::PgCatalogueRepository;
use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::{build_router, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mekhan_service=info,tower_http=info".into()),
        )
        .init();

    let config = AppConfig::load().expect("failed to load configuration");
    tracing::info!(
        "starting mekhan-service on {}:{}",
        config.host,
        config.port
    );

    let db = db::create_pool(&config.database_url).await?;
    tracing::info!("database connected and migrations applied");

    let petri = PetriClient::new(&config.petri_lab_url);

    let mekhan_nats = MekhanNats::connect(&config.nats_url, config.nats_creds.as_deref()).await?;
    tracing::info!("NATS connected at {}", config.nats_url);

    // Create catalogue subscription manager (KV-backed, in-memory cached)
    let sub_kv = mekhan_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("failed to create CATALOGUE_SUBSCRIPTIONS KV bucket");
    let subscription_manager = Arc::new(SubscriptionManager::new(
        sub_kv,
        mekhan_nats.jetstream().clone(),
    ));
    subscription_manager
        .hydrate()
        .await
        .expect("failed to hydrate catalogue subscriptions");
    subscription_manager
        .clone()
        .start_watcher()
        .await
        .expect("failed to start catalogue subscription watcher");
    tracing::info!("catalogue subscription manager ready");

    // Spawn lifecycle event listener (updates DB on NetCompleted/NetCancelled).
    // Triggers are wired in later once the dispatcher is built — see below.

    // Spawn background cleanup sweep
    tokio::spawn(lifecycle::start_cleanup_sweep(
        config.cleanup.clone(),
        db.clone(),
        mekhan_nats.clone(),
        petri.clone(),
    ));

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    tracing::info!("Yjs collaboration manager initialized");

    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    if let Err(e) = artifact_store.ensure_bucket().await {
        tracing::warn!("S3 bucket check failed (non-fatal): {e}");
    } else {
        tracing::info!("S3 artifact store ready (bucket: {})", config.s3.bucket);
    }

    let artifact_s3 = config.artifact_s3.as_ref().map(|cfg| {
        Arc::new(ArtifactStore::new(cfg))
    });

    // Live broadcasts for SSE fan-out of metric/log events.
    let live = mekhan_service::causality::live::LiveBroadcasts::new();

    // Trigger dispatcher — hydrates from every published template's
    // graph_json on boot. Background sources (cron, catalog, lifecycle,
    // webhook) hang off the same dispatcher in subsequent sub-phases.
    // Created before the lifecycle listener + causality ingest so we can
    // hand it through; the ingest hook fires catalog triggers on new
    // artifacts and the lifecycle hook fires net_completion triggers.
    let trigger_dispatcher = mekhan_service::triggers::start_trigger_dispatcher(
        db.clone(),
        petri.clone(),
        mekhan_nats.clone(),
    )
    .await;
    tracing::info!("trigger dispatcher ready");

    tokio::spawn(lifecycle::start_lifecycle_listener(
        mekhan_nats.clone(),
        db.clone(),
        subscription_manager.clone(),
        Some(trigger_dispatcher.clone()),
    ));

    // Causality ingest (PETRI_GLOBAL domain events → causality tables)
    // Single projection path for processes, tasks, metrics, logs, and catalogue.
    tokio::spawn(mekhan_service::causality::ingest::start_causality_ingest(
        mekhan_nats.clone(),
        db.clone(),
        subscription_manager.clone(),
        live.clone(),
        Some(trigger_dispatcher.clone()),
    ));

    let catalogue_repo = Arc::new(PgCatalogueRepository::new(db.clone()));

    // Spawn catalogue NATS request-reply responder
    tokio::spawn(mekhan_service::catalogue::responder::start_catalogue_responder(
        mekhan_nats.clone(),
        catalogue_repo.clone(),
        subscription_manager.clone(),
    ));

    // Auth adapters — composition root chooses the implementation by config.
    // `TokenVerifier`/`PrincipalResolver` are reused unchanged: the BFF
    // callback verifies the IdP's token with them, then caches the resolved
    // `AuthUser`. The per-request hot path goes through the `Authenticator`.
    let token_verifier = build_token_verifier(&config).await?;
    let principal_resolver: Arc<dyn PrincipalResolver> =
        Arc::new(StaticPrincipalResolver);

    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));
    let (authenticator, oidc) =
        build_authenticator(&config, session_store.clone()).await?;

    // Background sweep of expired sessions + stale login flows.
    {
        let store = session_store.clone();
        let ttl = config.auth.session_ttl_secs;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tick.tick().await;
                match store.sweep_expired(ttl).await {
                    Ok(n) if n > 0 => {
                        tracing::info!("auth: swept {n} expired session/login rows")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("auth: session sweep failed: {e}"),
                }
            }
        });
    }

    let state = AppState {
        db,
        petri,
        nats: mekhan_nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3,
        catalogue_repo,
        live,
        authenticator,
        session_store,
        oidc,
        token_verifier,
        principal_resolver,
        triggers: trigger_dispatcher,
    };

    let app = build_router(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Build the `TokenVerifier` the BFF callback uses to verify the access token
/// the IdP returns (before caching the resolved `AuthUser`). In `dev_noop`
/// there is no IdP, so the noop verifier stands in.
async fn build_token_verifier(config: &AppConfig) -> anyhow::Result<Arc<dyn TokenVerifier>> {
    match config.auth.mode {
        AuthMode::Bff => {
            let issuer_url = config
                .auth
                .issuer_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.issuer_url"))?;
            let audience = config
                .auth
                .audience
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.audience"))?;
            let verifier = ZitadelTokenVerifier::new(&ZitadelConfig {
                issuer_url,
                audience,
            })
            .await
            .map_err(|e| anyhow::anyhow!("zitadel verifier init: {e}"))?;
            tracing::info!("auth: Zitadel token verifier ready (BFF callback)");
            Ok(Arc::new(verifier))
        }
        AuthMode::DevNoop => {
            guard_dev_noop()?;
            tracing::warn!("auth: NoopTokenVerifier active — every request becomes the dev user");
            Ok(Arc::new(NoopTokenVerifier::default()))
        }
    }
}

/// Build the per-request authenticator and (in `bff`) the OIDC client. Fails
/// fast on discovery just like `ZitadelTokenVerifier::new`.
async fn build_authenticator(
    config: &AppConfig,
    session_store: Arc<dyn SessionStore>,
) -> anyhow::Result<(Arc<dyn Authenticator>, Option<Arc<OidcClient>>)> {
    match config.auth.mode {
        AuthMode::Bff => {
            let issuer_url = config
                .auth
                .issuer_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.issuer_url"))?;
            let client_id = config
                .auth
                .client_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.client_id"))?;
            // The IdP redirect URI is fixed to the same-origin callback. The
            // SPA proxies /api to the backend in dev and is same-origin in
            // prod, so this is host-relative to the public origin — but the
            // IdP needs an absolute value. Take it from config-derived bootstrap
            // (written by deploy/zitadel/bootstrap.sh) via the post-login
            // origin if present, else assume same-origin localhost dev.
            let redirect_uri = std::env::var("MEKHAN__AUTH__REDIRECT_URI")
                .ok()
                .unwrap_or_else(|| "http://localhost:5173/api/auth/callback".to_string());

            let oidc = OidcClient::discover(OidcConfig {
                issuer_url,
                client_id,
                client_secret: config.auth.client_secret.clone(),
                redirect_uri,
                scopes: config.auth.scopes.clone(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("oidc discovery init: {e}"))?;
            let oidc = Arc::new(oidc);
            tracing::info!("auth: BFF authenticator ready (server-side OIDC)");
            Ok((
                Arc::new(BffAuthenticator::new(session_store, oidc.clone())),
                Some(oidc),
            ))
        }
        AuthMode::DevNoop => {
            guard_dev_noop()?;
            tracing::warn!("auth: NoopAuthenticator active — every request is the dev user");
            Ok((Arc::new(NoopAuthenticator::default()), None))
        }
    }
}

/// Refuse to boot a dev-mode credential bypass in production.
fn guard_dev_noop() -> anyhow::Result<()> {
    let prod = std::env::var("MEKHAN_ENV")
        .map(|v| v.eq_ignore_ascii_case("prod") || v.eq_ignore_ascii_case("production"))
        .unwrap_or(false);
    if prod {
        anyhow::bail!("auth.mode=dev_noop is forbidden when MEKHAN_ENV=prod");
    }
    Ok(())
}
