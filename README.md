# Chatflow

A Rust service for versioned JSON chat flows. The service supports authenticated flow publication, worker-backed chat turns, durable run events, and RAGFlow-backed retrieval. [The implementation plan](PLAN.md) records the delivered milestones and deployment choices.

See [the feature documentation](docs/README.md) for architecture, flow definitions, API behavior, operations, and testing.

## Local development

1. Copy `.env.example` to `.env` and load the values into your shell (for example, `set -a; source .env; set +a` in Bash or Zsh).
2. Start Postgres with `docker compose up -d postgres`. Run `docker compose up -d vault` for local provider credentials, `docker compose up -d otel-collector prometheus grafana` for telemetry, and `scripts/local-ragflow.sh` for the pinned official RAGFlow stack.
3. Run `cargo run --bin api` and `cargo run --bin worker` in separate terminals. All apply migrations at startup.
4. Check `GET /health/live` and `GET /health/ready`.
5. Create a tenant and scoped keys with `scripts/local-bootstrap.sh my-tenant`. It saves the application, RAGFlow configuration, and Prometheus keys in ignored `.local/` files.

For the local Svelte conversation and flow editor, run `cd web && npm ci && npm run dev`, then open `http://127.0.0.1:5173`. The development proxy reads `.local/api-key` on the server; the browser does not receive the key. See [Workbench usage](web/README.md).

For the complete local Compose environment, run `scripts/full-compose.sh up`. It starts the pinned official RAGFlow stack, creates and indexes a smoke dataset, boots Chatflow and its supporting services, and verifies both a mock turn and a RAGFlow-backed turn. `scripts/full-compose.sh reset` removes both projects' containers, named volumes, and generated local keys; running `up` again recreates them. This reset deletes local Chatflow and RAGFlow data. See [container operation](docs/operations.md#containerized-local-api-and-worker).

You can copy this repository to another machine without copying `.local/`, the local RAGFlow `docker/.env`, or Docker volumes. Install Docker with Compose 2.24.4 or newer, Bash, Python 3, and curl, then run `scripts/full-compose.sh up` from the new checkout. The startup script creates the local RAGFlow `docker/.env` from the checked-in `docker/.env.example`. The new machine still needs network access to pull container images and build the Rust image. See [moving the setup](docs/operations.md#moving-the-setup-to-another-machine).

The example flow uses the deterministic `mock` model alias. For `openai`, store the provider key in Vault, register its tenant-owned reference through the API, and set `OPENAI_MODEL`, `VAULT_ADDR`, and `VAULT_TOKEN` on workers. Set `OPENAI_BASE_URL` on workers for an OpenAI-compatible router. Configure a RAGFlow endpoint, API key, and tenant dataset mapping for the `rag.retrieve` node; see [operations](docs/operations.md). The OpenAI adapter uses [Rig's completion API](https://docs.rs/rig-core/latest/rig_core/completion/index.html). Set `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:14317` to send traces and run counters to the local collector.

## Chat API

Use `Authorization: Bearer <api_key>` on every `/v1` request. A key needs `flows:write` to publish, `flows:read` to list versions, `runs:write` to start turns, and `runs:read` to inspect them. The bootstrap application key also grants `rag:read`, credential management, and key rotation. Separate keys grant `rag:configure` for dataset assignment and `ops:read` for metrics. Rotate an application token with `POST /v1/api-keys/rotate`, then revoke the old token by public ID. Revoking a key family in `api_keys` immediately blocks all its tokens.

- `POST /v1/flows/{flow_id}/versions` accepts `{ "name": "Basic chat", "definition": <flow JSON> }` and returns an immutable `version_id`.
- `POST /v1/runs` accepts `{ "flow_version_id": "...", "input": { "message": "Hello" } }`. Pass `conversation_id` to append a turn to an existing conversation.
- `GET /v1/runs/{run_id}` returns status, the pinned flow version, final output, and error.
- `POST /v1/runs/{run_id}/cancel` cancels an active run.
- `GET /v1/runs/{run_id}/events` streams ordered SSE events. Reconnect with `Last-Event-ID` to resume after an event sequence.
- `GET /v1/conversations/{conversation_id}/messages` returns stored user and assistant messages.
- `GET /v1/conversations/{conversation_id}/variables` lists private JSON variables; `PUT` or `DELETE /v1/conversations/{conversation_id}/variables/{name}` updates them. Flow nodes `variable.get` and `variable.set` can read and write string values during a turn.

Conversations and their runs, messages, and variables belong to one API key family. Give each end user a separate key family to keep users isolated; rotated tokens of the same key retain access. A new conversation starts with no variables.

See [load testing](docs/testing.md#load-testing) for the 20-concurrent-chat benchmark harness.

The API key is supplied in an HTTP header; the flow JSON only refers to reviewed node types and server-configured model aliases. Token events are currently stored durably, which makes reconnects exact but may be costly at high token rates.

Run `cargo test --all-targets`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check` before merging. The end-to-end test starts API and two worker processes when `DATABASE_URL` points to a test Postgres database; CI supplies one. See `examples/basic-chat.json` and `examples/rag-chat.json` for valid version 1 flows. `schemas/catalog.schema.json` contains the flow and reviewed node schemas; regenerate it with `cargo run --example print_schema > schemas/catalog.schema.json`.
