use std::net::SocketAddr;
use std::sync::Arc;

use mekhan_service::config::AppConfig;
use mekhan_service::db;
use mekhan_service::lifecycle;
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::s3::ArtifactStore;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
use mekhan_service::{build_router, process, AppState};

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

    let mekhan_nats = MekhanNats::connect(&config.nats_url).await?;
    tracing::info!("NATS connected at {}", config.nats_url);

    // Spawn lifecycle event listener (updates DB on NetCompleted/NetCancelled)
    tokio::spawn(lifecycle::start_lifecycle_listener(
        mekhan_nats.clone(),
        db.clone(),
    ));

    // Spawn background cleanup sweep
    tokio::spawn(lifecycle::start_cleanup_sweep(
        config.cleanup.clone(),
        db.clone(),
        mekhan_nats.clone(),
        petri.clone(),
    ));

    // Spawn catalogue ingest (NATS → Postgres)
    tokio::spawn(mekhan_service::catalogue::ingest::start_catalogue_ingest(
        mekhan_nats.clone(),
        db.clone(),
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

    // Process tracking ingest (NATS → Postgres)
    let nats_proc = mekhan_nats.clone();
    let db_proc = db.clone();
    tokio::spawn(async move {
        process::ingest::start_process_event_ingest(nats_proc, db_proc).await;
    });

    let nats_metric = mekhan_nats.clone();
    let db_metric = db.clone();
    tokio::spawn(async move {
        process::ingest::start_metric_ingest(nats_metric, db_metric).await;
    });

    let nats_log = mekhan_nats.clone();
    let db_log = db.clone();
    tokio::spawn(async move {
        process::ingest::start_log_ingest(nats_log, db_log).await;
    });

    let state = AppState {
        db,
        petri,
        nats: mekhan_nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3,
    };

    let app = build_router(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
