# Rustflow Workbench

A local Svelte workspace for building flow definitions and inspecting conversations. It has no login screen. Vite binds to `127.0.0.1` and proxies API requests using a local key file; the key is never sent to browser JavaScript or stored in the browser.

Start the API and worker as described in the repository README, bootstrap a tenant, then run:

```sh
cd web
npm ci
npm run dev
```

Open `http://127.0.0.1:5173`. The default proxy reads `../.local/api-key` and calls `http://127.0.0.1:3000`. Set `CHATFLOW_API_KEY_FILE` to another local key file and `CHATFLOW_API_URL` to another API origin when testing a separate environment. Keep this proxy loopback-only and use a key with `flows:read`, `flows:write`, `runs:read`, and `runs:write`; RAG flows also need `rag:read`.

The Flows workspace loads published versions as draft copies. You can add every reviewed node type, edit its configuration, bindings, guard, retries and timeout, edit flow inputs, outputs and limits, inspect the graph or full JSON, validate, dry-run, and publish a new immutable version. The graph is an editor for node fields and bindings, not a drag-and-drop connector; JSON mode remains available for exact inspection and import.

The Conversations workspace lists the latest 100 accessible conversations, shows messages, streams new turns, and exposes run node attempts and events. It also supports persisted conversation notes and variables. The run list is limited to the latest 100 runs per conversation. Existing API key family and tenant isolation still apply.

Verify the UI with `npm run check`, `npm test`, and `npm run build`. The browser UI is intended for trusted local development; it is not a public deployment or a replacement for user authentication.
