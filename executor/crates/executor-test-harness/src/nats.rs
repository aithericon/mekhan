use async_nats::jetstream;
use testcontainers::runners::AsyncRunner;
use testcontainers::ImageExt;
use testcontainers_modules::nats::{Nats, NatsServerCmd};
use tokio::sync::OnceCell;

struct SharedNats {
    url: String,
    client: async_nats::Client,
    jetstream: jetstream::Context,
    _container: testcontainers::ContainerAsync<Nats>,
}

static SHARED_NATS: OnceCell<SharedNats> = OnceCell::const_new();

async fn shared_nats() -> &'static SharedNats {
    SHARED_NATS
        .get_or_init(|| async {
            let cmd = NatsServerCmd::default().with_jetstream();
            let container = Nats::default()
                .with_cmd(&cmd)
                .start()
                .await
                .expect("Failed to start NATS testcontainer");

            let host = container.get_host().await.expect("get_host");
            let port = container.get_host_port_ipv4(4222).await.expect("get_port");
            let url = format!("nats://{host}:{port}");

            let client = async_nats::connect(&url)
                .await
                .expect("connect to shared NATS testcontainer");
            let jetstream = jetstream::new(client.clone());

            SharedNats {
                url,
                client,
                jetstream,
                _container: container,
            }
        })
        .await
}

/// Returns the NATS URL for the shared testcontainer, starting it on first call.
pub async fn shared_nats_url() -> &'static str {
    &shared_nats().await.url
}

/// Returns a clone of the NATS client from the shared testcontainer.
pub async fn shared_nats_client() -> async_nats::Client {
    shared_nats().await.client.clone()
}

/// Returns a JetStream context from the shared testcontainer.
pub async fn shared_jetstream() -> jetstream::Context {
    shared_nats().await.jetstream.clone()
}
