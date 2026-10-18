# HTTP API

All `/v1` endpoints require `Authorization: Bearer <application API key>`. Keys are tenant scoped and carry `flows:read`, `flows:write`, `runs:write`, `runs:read`, `rag:read`, `rag:configure`, `credentials:read`, `credentials:write`, `keys:read`, `keys:write`, or `ops:read` scopes. Optional key restrictions limit flows and model aliases. A revoked or expired key cannot make new requests. Conversation ownership is the stable API key ID, shared by its rotated tokens. Issue a distinct API key family per end user; callers sharing a key family share access to that user's conversations.

| Method and path | Scope | Purpose |
| --- | --- | --- |
| `GET /v1/flows` | `flows:read` | List tenant flows and latest versions |
| `POST /v1/flows/dry-run` | `flows:write` | Execute a draft with simulated model calls and return node outputs |
| `POST /v1/flows/validate` | `flows:write` | Validate a draft definition without publishing |
| `POST /v1/flows/{flow_id}/versions` | `flows:write` | Validate and publish an immutable flow version |
| `GET /v1/flows/{flow_id}/versions` | `flows:read` | List published versions |
| `GET /v1/flows/{flow_id}/versions/{version}` | `flows:read` | Read one published definition |
| `POST /v1/provider-credentials` | `credentials:write` | Register or rotate a tenant-owned Vault reference for `openai` |
| `GET /v1/provider-credentials` | `credentials:read` | List tenant credential IDs and aliases |
| `GET /v1/ragflow-datasets` | `rag:configure` | List tenant RAGFlow dataset IDs |
| `PUT /v1/ragflow-datasets` | `rag:configure` | Replace tenant RAGFlow dataset IDs |
| `POST /v1/api-keys/rotate` | `keys:write` | Issue one additional token for the authenticated key |
| `GET /v1/api-keys/tokens` | `keys:read` | List public token IDs and revocation state for the authenticated key |
| `POST /v1/api-keys/tokens/{public_id}/revoke` | `keys:write` | Revoke one token belonging to the authenticated key |
| `GET /v1/ops/metrics` | `ops:read` | Tenant-scoped Prometheus metrics for backlog, failures, tokens, and optional estimated cost |
| `POST /v1/runs` | `runs:write` | Create a conversation turn and queue its run |
| `GET /v1/conversations` | `runs:read` | List the latest 100 accessible conversations for this key family |
| `GET /v1/conversations/{conversation_id}/runs` | `runs:read` | List the latest 100 runs in one owned conversation |
| `GET /v1/runs/{run_id}` | `runs:read` | Read status, pinned version, output, or error |
| `POST /v1/runs/{run_id}/cancel` | `runs:write` | Cancel a queued or running run |
| `GET /v1/runs/{run_id}/events` | `runs:read` | Stream ordered SSE events |
| `GET /v1/runs/{run_id}/timeline` | `runs:read` | Read all durable events and node attempts |
| `GET /v1/conversations/{conversation_id}/messages` | `runs:read` | Read stored messages |
| `GET /v1/conversations/{conversation_id}/notes` | `runs:read` | List persisted notes in an owned conversation |
| `POST /v1/conversations/{conversation_id}/notes` | `runs:write` | Add a note with `{"body":"..."}` |
| `PATCH /v1/conversations/{conversation_id}/notes/{note_id}` | `runs:write` | Replace the note body |
| `DELETE /v1/conversations/{conversation_id}/notes/{note_id}` | `runs:write` | Delete a note |
| `GET /v1/conversations/{conversation_id}/variables` | `runs:read` | List the conversation's JSON variables |
| `PUT /v1/conversations/{conversation_id}/variables/{name}` | `runs:write` | Set one JSON variable with `{"value": ...}` |
| `DELETE /v1/conversations/{conversation_id}/variables/{name}` | `runs:write` | Remove one variable |

`POST /v1/runs` accepts `flow_version_id`, `input`, and optional `conversation_id`. The input fields and JSON types must match the published flow. The response returns `run_id`, `conversation_id`, `flow_version_id`, and initial status.

A conversation can be continued, read, or updated only with its owner's key family and current flow permissions. A different key, even in the same tenant, receives HTTP 404 for the conversation, its variables, messages, and runs. Existing conversations created before ownership was recorded are assigned to their earliest run's API key when possible; rows without a known key are inaccessible through these endpoints. Variable names contain 1–64 ASCII letters, digits, or underscores. Each conversation supports up to 64 variables, each with a JSON value of at most 16 KiB. `GET` returns `{"conversation_id":"...","variables":{"name":value}}`; a missing conversation or variable returns HTTP 404. Variables persist across runs in that conversation and are absent from new conversations.

The conversation browser excludes conversations containing any flow version the key may no longer read. A conversation's run list includes its version, status, input, output, error, and timestamps. Notes belong to the same tenant and conversation and remain available to rotated tokens of the owner key family. Note bodies must contain non-whitespace text and be at most 10,000 UTF-8 bytes. A missing note or a conversation owned by another key returns HTTP 404. Notes are intended for operator annotations, not provider secrets.

`POST /v1/provider-credentials` accepts `{"provider_alias":"openai","secret_reference":"tenants/TENANT_UUID/openai"}` and returns `credential_id` and `provider_alias`. Write the provider key to Vault KV v2 before registration; the API accepts a reference only. The reference must start with the authenticated tenant's UUID path. Registering the same alias again rotates its reference while retaining the ID. Draft validation and publication reject an `openai` flow whose credential ID does not belong to that tenant and alias. Listing never returns the Vault reference or secret.

`POST /v1/api-keys/rotate` returns a new `api_key` once, plus its public ID and the shared key ID. The old token continues to work until its public ID is revoked. Every token for that key shares its scopes, allowed flows and models, expiry, and rate, concurrency, token, and spend budgets; rotation cannot reset those counters. Store the returned token immediately, verify it works, then revoke the old public ID with `POST /v1/api-keys/tokens/{public_id}/revoke`. The token list exposes public IDs and timestamps only. Revoking a token does not cancel runs already accepted. Revoking the parent `api_keys` row in the database disables every token in the family.

Clients may send an `Idempotency-Key` header on run creation. A repeat with the same key and request returns the existing run with HTTP 200. Reusing a key for a different flow version, input, or explicit conversation returns HTTP 409. Keys are tenant scoped and limited to 128 ASCII letters, digits, hyphens, underscores, periods, or colons.

Cancellation returns `{"run_id":"...","status":"cancelled"}`. A terminal run returns HTTP 409. A cancellation event is appended to the run timeline.

Each SSE event includes a sequence ID, run and optional node ID, event type, status, timestamp, and structured metadata. Send `Last-Event-ID` on reconnection to receive only later events. Token text appears in `token` event metadata; the final answer is available from the run and conversation endpoints.

The timeline endpoint returns the durable event list plus node attempt statuses, outputs, errors, and timestamps. It is intended for debugging a run without server logs. Flow validation returns HTTP 422 with a specific error message when the definition is invalid.

`GET /health/live` checks the API process. `GET /health/ready` checks Postgres connectivity.

RAG runs and RAG dry runs additionally require `rag:read`. Reading a RAG run, its timeline or events, or its conversation also requires that scope. The `rag.retrieve` node emits a `rag.sources` event with each source's RAGFlow dataset, document, and chunk IDs. Its output includes those IDs alongside a context string for downstream prompt nodes. Document management is performed in RAGFlow.

`GET /v1/ragflow-datasets` lists the calling tenant's RAGFlow dataset IDs. `PUT /v1/ragflow-datasets` replaces them with `{"dataset_ids":["DATASET_ID"]}`. Both require `rag:configure`. IDs must be unique and contain 1–128 letters, digits, underscores, or hyphens; at most 32 IDs may be configured. An empty list disables RAG retrieval for the tenant. Changes take effect on the next retrieval without restarting processes. Create, upload, and parse documents in RAGFlow.

Issue `rag:configure` only to a trusted operator key after verifying that the dataset belongs to the tenant. A key with this scope can name any dataset readable by the shared RAGFlow key; the API cannot independently verify RAGFlow ownership. Ordinary tenant keys need only `rag:read`.

Run creation enforces tenant and API-key limits on queued/running runs and runs created in the past minute. The default key limits are 20 concurrent runs and 60 runs per minute; tenant defaults are 100 and 1,000. Exceeding a limit returns HTTP 429. Each run reserves input and output token allowances for every model node and permitted retry against rolling one-day tenant and key budgets. Defaults are 20 million input and 1 million output tokens per key, and 200 million input and 10 million output tokens per tenant. The declared prompt byte ceiling plus a small chat envelope is used as a conservative input-token allowance. Reservations remain counted for the day even when a run uses fewer tokens or fails.

`openai` transport runs, including router-backed Bedrock models, reserve estimated spend using `MODEL_INPUT_USD_PER_MILLION` and `MODEL_OUTPUT_USD_PER_MILLION` on the API. They are rejected with HTTP 503 until both prices are configured. The legacy `OPENAI_*` price pair remains a fallback when neither `MODEL_*` value is set; partial pairs are rejected. Default rolling daily spend ceilings are $10 per key and $100 per tenant, stored as integer microdollars. Operators may adjust the `max_*` columns in `api_keys` and `tenants`. These budgets use configured prices and reserved prompt sizes; provider invoices and reported token usage may differ.

`POST /v1/flows/dry-run` accepts `{"definition": <flow JSON>, "input": <flow input>}` and returns final outputs plus each node's status and output. It uses the deterministic mock model for every `llm.chat` node, regardless of the configured alias. `rag.retrieve` calls the configured RAGFlow service using the tenant's dataset mapping. The dry run does not create a conversation, run, or durable events.
