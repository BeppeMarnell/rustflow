# Rust chatflow framework: implementation plan

## Goal

Build a code-owned, dynamically configurable chatflow service that can run multiple conversations and workers safely. Flow definitions are data (versioned JSON); executable node types are reviewed Rust code. The first release should serve one real chat/RAG use case before becoming a general builder.

## Decisions for the first release

- Use one Cargo package with two binaries (`api` and `worker`) and shared modules. Split into crates only when module boundaries are proven.
- Use Axum and Tokio for the API and streaming; SQLx and Postgres for durable state and a first job queue; Serde, JSON Schema, and petgraph for flow definition and validation; Rig behind our own model-provider trait; `tracing` and OpenTelemetry for telemetry.
- Queue **runs**, rather than every node, for interactive chat. One worker executes a turn, checkpoints after nodes, and streams events back to the API. Parallel independent nodes can execute within that worker. Long-running work can use separate worker pools when needed.
- Use Postgres row claiming (`FOR UPDATE SKIP LOCKED`) with a lease, heartbeat, retry count, and idempotency key. Evaluate NATS JetStream after measuring queue contention or a need for independently scaled worker types.
- Pin each run to an immutable flow version. Published flows are validated and cannot be edited in place.
- Keep message and run retention disabled until an operator sets per-tenant retention days. Workers then remove expired messages and terminal runs with their events and checkpoints in bounded batches.
- Keep provider credentials server-side. Clients authenticate to our API; the service proxies approved calls. Never send a provider key to a browser or place it in flow JSON, queued jobs, logs, or traces.
- The first model provider is OpenAI through Rig. Tenant-owned credential IDs in flow definitions resolve to namespaced Vault KV v2 references in workers; a local Vault dev container is available through Compose. The mock alias remains for repeatable tests.

## System shape

```text
Client -> Axum API -> authenticate/authorize -> create run in Postgres
                                      |                    |
                                      |                    v
                                      |              worker claims run
                                      |                    |
                                      |              execute nodes + checkpoints
                                      |                    |
                                      +<-- event stream ---+
                                            SSE response

Postgres: tenants, API keys, provider credential references, flow versions,
          conversations, messages, runs, node runs, run events, jobs, RAG source references
RAG: RAGFlow owns document ingestion, parsing, embeddings, indexing, and retrieval
Telemetry: OpenTelemetry traces/metrics plus durable run events
```

## Core data contracts

- `FlowDefinition`: schema version, flow ID, declared inputs/outputs, nodes, edges, limits.
- `NodeDefinition`: stable ID, registered type, configuration, input bindings, retry/timeout policy.
- `NodeHandler`: validate configuration and execute typed input into typed output. Node handlers receive a constrained context with tenant, run, trace, and approved service handles.
- Initial node catalog: `input`, `prompt.render`, `llm.chat`, `condition`, `rag.retrieve`, `output`. Add `tool.call` and `http.request` only with explicit allowlists and bounds.
- `RunEvent`: monotonic sequence, run/node IDs, event type, timestamp, status, structured metadata. Events are durable; high-volume token chunks may be transient, while final messages are durable.
- Every node type declares JSON Schemas for configuration, inputs, and outputs. Validate flows on publication and node results at runtime. Branch expressions use a restricted expression language such as CEL, not arbitrary code.

## Security boundary

- Generate high-entropy application API keys. Store only a digest plus public key ID, tenant, scopes, allowed flows/models, expiry, and revocation state. Show secrets once; support rotation with overlap.
- Axum authentication middleware resolves the tenant and scopes. Every lookup for a flow, run, conversation, or credential checks tenant ownership. Management endpoints require separate scopes from run endpoints.
- The model router uses configured provider/model aliases, never a caller-supplied URL. Enforce per-key and per-tenant request rate, concurrent runs, token limits, and spend limits across workers.
- Store provider credentials in a secret manager or encrypted with a managed key. Flow definitions reference credential IDs. Restrict which worker roles can resolve them.
- For any future HTTP node, enforce destination allowlists and block private, loopback, link-local, and metadata-service destinations. Prevent redirects to disallowed destinations.
- Redact authorization headers and secrets from logs/traces. Prompt and response capture is opt-in; traces always contain structural IDs, timing, errors, and usage.
- Browser clients use application sessions or short-lived scoped credentials, not long-lived application API keys.

## Milestones

### 0. Foundation and contracts

Create the Cargo project, local Postgres setup, migrations, configuration loader, basic health endpoints, and flow JSON schema. Define the `NodeHandler` and provider interfaces. Write one example flow definition. Establish formatting, linting, and CI checks.

**Done when:** `cargo test` and `cargo clippy` pass; invalid graphs (unknown node, bad binding, cycle, missing output) are rejected before publication; an example flow validates.

### 1. First end-to-end chat turn

Implement API-key authentication, flow publication/versioning, run creation, worker leasing, `input -> prompt.render -> llm.chat -> output`, SSE token streaming, conversation/message persistence, and basic traces. Use one provider adapter and a mock provider for repeatable tests.

**Done when:** two worker processes can serve concurrent chats; each run records its flow version and node transitions; reconnecting clients can fetch final messages and run status; revoking a key prevents new runs.

### 2. Reliability and control flow

Add bounded concurrency, provider timeouts and retries, idempotent node commits, lease recovery, cancellation, conditional branches, parallel fan-out/join, and run replay from checkpoints. Add per-key quotas and model allowlists.

**Done when:** killing a worker mid-run does not lose the run; non-idempotent side effects are not repeated without an explicit policy; load tests report queue time, first-token latency, p95 completion latency, error rate, and provider throttling at a chosen target concurrency.

### 3. RAG

Integrate RAGFlow through the `rag.retrieve` node. RAGFlow owns document ingestion, parsing, embeddings, indexing, retrieval, and reindexing. Keep its API key server-side, bind each tenant to an operator-configured dataset list, and record returned dataset, document, and chunk IDs in run events.

**Done when:** a tenant can answer from its configured RAGFlow datasets with source IDs; another tenant cannot select or read those sources; document updates are handled in RAGFlow without a second local indexing pipeline.

### 4. Developer experience and operations

Expose a flow management API, validation errors, dry-run/debug endpoint, and a run timeline endpoint. Add dashboards/alerts for worker backlog, provider failures, token usage, and cost. Decide whether a visual editor is useful after the JSON/API workflow is exercised.

**Done when:** a developer can publish a new flow version and inspect one run from request through every node without reading server logs.

## Current acceptance status

Milestones 0–4 have local implementation and automated coverage. A local mock load benchmark is recorded in `benchmarks/2026-09-28-local-mock.json`; [a three-request local Haiku load check](benchmarks/2026-09-29-live-haiku-load.json) reports p95 queue, first-token, and completion latencies at two concurrent chats with zero errors or throttles. A production latency objective still depends on the deployment and target concurrency. Milestone 3 uses the official RAGFlow Docker stack for ingestion, embeddings, indexing, and retrieval. The adapter and tenant isolation have automated coverage through a mock RAGFlow endpoint, and tenant dataset assignments are held in Postgres. [The live RAGFlow smoke record](benchmarks/2026-09-29-ragflow-smoke.json) confirms document parsing and retrieval. The private [`chatflow` dataset and live run record](benchmarks/2026-09-29-chatflow-dataset.json) confirm a complete Chatflow run against a parsed RAGFlow document, including the verification phrase and a `rag.sources` event with matching dataset, document, and chunk IDs. [A revised run](benchmarks/2026-09-29-question-aware-rag.json) confirms that the model prompt contains both the original question and RAGFlow context. [The current source-only dry run](benchmarks/2026-09-29-source-only-rag.json) confirms that the question occurs exactly once in the prompt. These earlier RAG runs use the deterministic mock model. The OpenAI-compatible router adapter has a full API-to-worker integration test with fake Vault, RAGFlow, and router servers; [the router check](benchmarks/2026-09-29-router-check.json) separately confirms a direct streaming completion with the Claude Haiku Bedrock inference profile and token usage. [The live Haiku RAG run](benchmarks/2026-09-29-live-haiku-rag.json) confirms authorized tenant credential registration, real RAGFlow retrieval, a successful Bedrock Haiku reply containing the verification phrase and source IDs, and reported token usage. Its $100-per-million-token local admission rates are synthetic test ceilings, not provider prices.

## Choices to settle during the first slice

- The initial provider and credential boundary are implemented with OpenAI-compatible Chat Completions, Vault, and tenant-owned references. The local router test selected `bedrock/eu.anthropic.claude-haiku-4-5-20251001-v1:0`; obtain contracted input and output pricing before production deployment.
- Target concurrent chats, latency objective, and deployment environment; these determine the load-test threshold, not the framework API.
- The first RAG flow is `examples/rag-chat.json` and has been exercised against RAGFlow locally. Choose the production content and model for deployment.
- Deployment-specific retention periods for conversation messages and terminal runs. The first-release default is indefinite retention until each tenant is configured; prompt and response trace capture is disabled.

The optional Docker Compose `app` profile now runs the API and worker alongside local dependencies. [Its smoke record](benchmarks/2026-09-29-container-rag.json) confirms a successful RAGFlow-to-Haiku turn, non-root execution, a readable mounted secret, and collector run metrics. This local profile is not a production environment; deployment target, contracted model rates, sustained load target, and retention settings remain to be chosen.


The pinned RAGFlow Docker files are vendored in the repository, and the two Compose projects share a named Docker network. [The portability check](benchmarks/2026-09-29-portability.json) rebuilt the complete stack from a clean copy in another filesystem path and verified a RAGFlow-backed run over the named network. A different physical host and native Linux have not been tested.

The local Compose app now has a staged provisioning script and an in-image bootstrap binary. `scripts/full-compose.sh` also provisions the pinned official RAGFlow stack through its admin and public APIs: it creates an API key, an embedding dataset, a parsed document, and the tenant dataset assignment. In isolated projects, both Chatflow and RAGFlow containers and named volumes were removed, then recreated from empty state; the complete mock and RAGFlow-backed runs passed after the rebuild. [The full rebuild record](benchmarks/2026-09-29-full-compose-rebuild.json) captures the sequence. Live model credentials remain external secrets and must be restored after a Vault dev reset.
