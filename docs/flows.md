# Flow definitions

`FlowDefinition` is a versioned JSON document with `schema_version`, `flow_id`, declared `inputs` and `outputs`, `nodes`, `edges`, and `limits`. See [the example chat flow](../examples/basic-chat.json), [the RAG chat flow](../examples/rag-chat.json), and [the generated schema catalog](../schemas/catalog.schema.json).

Node IDs are stable within a flow version. A binding names a source node and one of its outputs. The `edges` array must agree exactly with the bindings. Publication rejects unknown node types, invalid configuration, missing inputs and outputs, unknown sources, incompatible types, cycles, and invalid limits.

`retry.max_attempts` is the number of additional attempts after the first call. The default is zero. A positive value explicitly permits replaying an interrupted `llm.chat` call and retrying transient model failures or timeouts. A node's `timeout_ms` limits one attempt. `limits.max_run_ms` caps total execution time from the first worker start and remains in force after a lease recovery.

`limits.max_parallel_nodes` defaults to 4 and may be set from 1 to 32. Independent ready nodes run in the same worker up to that limit. The worker waits for the batch before scheduling dependent nodes.

The currently executable nodes are:

| Type | Configuration | Input | Output |
| --- | --- | --- | --- |
| `input` | `name` of a declared string flow input | none | `value` string |
| `prompt.render` | `template`; replaces `{{text}}` and optional `{{question}}` once, without expanding placeholders inside input values | `text` string, optional `question` string | `value` string |
| `llm.chat` | server `model_alias`, `max_output_tokens` from 1 to 4096, optional `max_prompt_bytes` from 1 to 65536 (default 16384), `credential_id` UUID required for `openai` | `prompt` string | `value` string |
| `condition` | `equals` string | `text` string | `value` boolean |
| `select` | empty object | `condition` boolean, `when_true` and `when_false` strings or skipped values | `value` string |
| `rag.retrieve` | `top_k` from 1 to 10 | `query` string | `context` source-only string; legacy `value` string with question and sources; `sources` with RAGFlow dataset, document, and chunk IDs |
| `variable.get` | `name` of a conversation variable | none | `value` string; run fails if absent or not a string |
| `variable.set` | `name` of a conversation variable | `value` string | `value` string written to that conversation |
| `output` | empty object | `value` string | `value` string |

Variable names contain 1–64 ASCII letters, digits, or underscores. A `variable.set` node stores at most 16 KiB and each conversation holds at most 64 variables. Values written through the HTTP API may be any JSON value; `variable.get` reads string values only. A variable node requires a persisted conversation, so dry runs containing one return HTTP 422. Concurrent runs on one conversation observe the latest committed value at the time each node executes; writes to the same name use last commit wins. The variable nodes' outputs are visible in that conversation owner's run timeline.

Published versions cannot be updated or deleted. Runs retain the exact version ID used when they were created.

The RAG example binds `question` to the original user message and `text` to the source-only `context` output. A template that uses `{{question}}` must bind that input; publication rejects it otherwise. The older `value` output still includes the question so existing published flows keep their prompt behavior.

The worker rejects a model prompt whose UTF-8 byte length exceeds `max_prompt_bytes`. Set a larger bound on a RAG flow if its retrieved context needs it. Admission reserves that byte limit plus a small chat envelope as a conservative input-token allowance, and reserves the declared output-token maximum. Both reservations include every permitted retry attempt.

Set `run_if` on a node to `{ "node": "condition-node-id", "equals": true }` or `false`. A node whose guard does not match is checkpointed as `skipped`. The `select` node waits for both branch sources, then returns the selected branch's value. A skipped source is represented internally as `null`; selecting it fails the run. Guard dependencies participate in cycle validation. See [the conditional example](../examples/conditional-chat.json).

## Workbench editor

The local Svelte workbench visualizes nodes and bindings as a graph and exposes forms for every reviewed node type, binding, guard, retry policy, timeout, flow input, output, and limit. It derives `edges` from node bindings so the graph and JSON stay consistent. JSON mode allows direct inspection and import of the full definition. Validate or dry-run a draft before publishing; opening a published version makes a draft copy, and publishing always creates a new immutable version. See [Workbench usage](../web/README.md).
