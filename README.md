# Mekhan

SOP workflow management — Petri-net backed, real-time collaborative editing.

## Structure

| Directory | Description |
|-----------|-------------|
| [`app/`](./app/) | SvelteKit frontend (Svelte 5, xyflow, Yjs) |
| [`service/`](./service/) | Rust backend (Axum, Postgres, NATS, Yjs) |
| [`docs/`](./docs/) | Architecture & migration planning |
| [`docker-compose.yml`](./docker-compose.yml) | Local infra (Postgres + NATS) |

## Quick start

```bash
# Start infrastructure
docker compose up -d

# Start backend
cd service && cargo run

# Start frontend (separate terminal)
cd app && npm install && npm run dev
```
