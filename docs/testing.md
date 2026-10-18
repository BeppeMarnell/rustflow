# Testing

`tests/conversation_variables.rs` covers variable persistence across turns, flow node reads and writes, isolation between conversations, and denial of access to a second key in the same tenant. It runs when `DATABASE_URL` points to Postgres and starts local API and worker processes.

`tests/workbench_api.rs` covers conversation listing, run browsing, note validation and CRUD, same-tenant key isolation, cross-tenant isolation, and flow permission changes. It requires `DATABASE_URL`. In `web/`, run `npm ci`, `npm run check`, `npm test`, and `npm run build`; the TypeScript tests cover edge derivation, node-reference edits, JSON import bounds, and chunked SSE parsing.

## Compose rebuild check

`scripts/full-compose.sh up` provisions the official RAGFlow stack and Chatflow application from empty volumes. It creates and parses a RAGFlow document, verifies retrieval, publishes deterministic mock and RAG flows, and asserts the worker returns the expected replies. `scripts/full-compose.sh reset` removes both Compose projects' named volumes and generated keys. The isolated rebuild check ran `reset` and `up` again, creating a new RAGFlow dataset and Chatflow tenant; both flows succeeded. For app-only checks, use `scripts/compose-environment.sh up|verify|reset` with separate `CHATFLOW_COMPOSE_PROJECT`, `CHATFLOW_STATE_DIR`, and port overrides listed in [operations](operations.md#containerized-local-api-and-worker). Use a dedicated Postgres database for Rust integration tests while a Compose worker is running, because the worker can claim jobs in the same database.


The portability check copied only source files, including the vendored RAGFlow Docker configuration, to a different path with no `.local/` state or Docker volumes. `scripts/full-compose.sh up` created both projects and passed both flows. A separate app project in that relocated copy then retrieved from the original RAGFlow container over the named Docker network. See [the portability record](../benchmarks/2026-09-29-portability.json). This checks path independence on Docker Desktop; native Linux remains untested.

Every new feature needs a focused unit test that checks its behavior and a relevant failure or boundary case. Database transactions, lease behavior, tenant isolation, and API behavior also need Postgres integration coverage.

Run `cargo fmt --all -- --check`, `cargo test --all-targets --locked`, and `cargo clippy --locked --all-targets -- -D warnings`. The end-to-end test in `tests/end_to_end.rs` runs only when `DATABASE_URL` is set. It starts the API and two worker processes, checks run events and messages, and verifies key revocation. CI provisions Postgres and sets `DATABASE_URL`. Without it, database tests return early and do not verify SQL behavior.

Unit tests cover worker concurrency settings, cancellation eligibility, model retry budgets, total run deadlines, interrupted-call replay policy, idempotency key validation, quota and reservation calculations, model-price pair precedence and partial-pair rejection, literal prompt rendering of question and source values, source-only RAG context with legacy output compatibility, condition selection, parallel readiness, retention sweep settings, telemetry endpoint validation, Vault configuration, KV v2 parsing, credential-reference bounds, custom OpenAI-compatible endpoint validation, and a bearer-authenticated streaming Chat Completions exchange. The Postgres tests also check queued cancellation, checkpoint recovery after an expired lease, worker process crash recovery, exhausted lease cleanup, concurrent quota admission, tenant-specific retention, idempotent run creation, flow management reads, the run timeline, skipped conditional branches, a fan-out/join flow, RAGFlow request/response handling and read-scope isolation for RAG runs. Credential tests verify tenant-scoped API listing, publication checks, and worker-side secret resolution.
The key-rotation test verifies old and new tokens overlap, per-key quotas stay shared across them, a different tenant cannot revoke a token, and each token stops authenticating after revocation. The end-to-end test verifies that a run key cannot assign RAGFlow datasets while a separate configuration key can.

`tests/router_flow.rs` starts the API and worker with temporary loopback Vault, RAGFlow, and OpenAI-compatible router servers. It registers a tenant credential through the scoped API, publishes a RAG flow, runs it, and checks both source IDs and streamed model usage in the timeline. This verifies the full integration path without a real provider charge.

When flow or node schemas change, run `cargo run --example print_schema > schemas/catalog.schema.json`. `tests/flow_validation.rs` checks that the checked-in schema matches the Rust contracts.

## Live RAGFlow check

Start the official local stack with `scripts/local-ragflow.sh`. Its CPU embedding service uses `BAAI/bge-small-en-v1.5` by default; `GET http://127.0.0.1:6380/health` should return 200. In RAGFlow, create a test dataset using that embedding model, upload [the sample document](../examples/ragflow-smoke.txt), and wait for RAGFlow parsing to finish. Create a RAGFlow API key, map the local chatflow tenant to the dataset with `PUT /v1/ragflow-datasets`, and run the `rag-chat.json` flow. A query for the verification phrase should produce a `rag.sources` event with the RAGFlow dataset, document, and chunk IDs. The mock RAGFlow integration test verifies the same source contract without depending on the local service.

[The September 29 smoke record](../benchmarks/2026-09-29-ragflow-smoke.json) confirms that the official local RAGFlow stack parsed the sample document and retrieved its verification phrase with all three source IDs. [The Chatflow live run record](../benchmarks/2026-09-29-chatflow-dataset.json) confirms that the published RAG flow completed, returned the phrase `cedar moon 417`, and emitted one `rag.sources` event with matching dataset, document, and chunk IDs. That run used the deterministic mock model after live RAGFlow retrieval; it does not measure a production model.

[The revised RAG run](../benchmarks/2026-09-29-question-aware-rag.json) confirms that the stored prompt contained the original question and RAGFlow context, and its output retained the verification phrase and source IDs. It used the mock model while live tenant credential registration remains pending.

[The source-only RAG dry run](../benchmarks/2026-09-29-source-only-rag.json) exercises the current example against the live RAGFlow dataset. Its source context omits the question, while the rendered prompt includes the question exactly once and retains the source IDs and verification phrase. The model response is simulated by the deterministic mock provider.

[The router check](../benchmarks/2026-09-29-router-check.json) records an initial edge error 1010 and a successful recheck with an explicit `chatflow/0.1` user agent. The selected `bedrock/eu.anthropic.claude-haiku-4-5-20251001-v1:0` inference profile returned a streamed `OK` response and usage. The server-side OpenAI-compatible Chat Completions adapter also passes a local streaming test. [The live Haiku RAG run](../benchmarks/2026-09-29-live-haiku-rag.json) confirms the complete API-to-worker path with the tenant credential registered through the authorized API, real RAGFlow source IDs, a model reply containing the verification phrase, and provider-reported token usage. The temporary `credentials:write` key was revoked after registration. Local API admission uses synthetic $100 input and output rates per million tokens solely for the smoke test; these do not represent billed prices.

## Load testing

With the API, workers, and a published chat flow running, set `CHATFLOW_API_URL`, `CHATFLOW_API_KEY`, and `CHATFLOW_FLOW_VERSION_ID`, then run `cargo run --example load_test`. The default workload is 100 requests at 20 concurrent chats; override with `LOAD_COUNT` and `LOAD_CONCURRENCY`. The JSON report includes p95 queue time, first-token latency, completion latency, error rate, and provider-throttle count. Use a key with quotas above the workload and a dedicated test tenant. The default key limit of 20 concurrent runs and 60 runs per minute will otherwise reject part of the default workload.

The [local mock benchmark](../benchmarks/2026-09-28-local-mock.json) records one run of the default workload. Its latency numbers describe that local machine and mock provider only; run the benchmark again with the intended provider and deployment before setting a production latency target.

[The local Haiku load check](../benchmarks/2026-09-29-live-haiku-load.json) records three requests at two concurrent chats through the live RAGFlow and Bedrock Haiku path. It reports p95 queue, first-token, and completion latencies, zero errors, and zero provider throttles. This small local sample does not establish a production latency target or sustained capacity.

[The container smoke record](../benchmarks/2026-09-29-container-rag.json) exercises the same published flow through the optional Compose `app` profile. It verifies API readiness, a non-root process, the readable secret mount, collector run metrics, RAGFlow source IDs, and a successful Haiku reply with token usage.

## Local operations check

[The local operations record](../benchmarks/2026-09-29-local-operations.json) confirms that the Chatflow API, embedding service, Prometheus, Grafana, and Jaeger responded; both Prometheus scrape targets were up; four Chatflow alert rules and the provisioned Grafana dashboard were loaded. These are local service and configuration checks, not evidence of production alert delivery.
