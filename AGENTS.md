# Repository instructions

## Scope

These instructions apply to the whole repository. `PLAN.md` describes the intended system and milestone acceptance criteria. Keep executable node types in reviewed Rust code; JSON flow definitions are data.

## Changes

- Keep the single Cargo package with `api` and `worker` binaries until a crate split has a concrete benefit.
- Preserve tenant scoping on every database read and write. Keep provider secrets in worker configuration; never put them in flow JSON, events, logs, or API responses.
- Treat published flow versions as immutable. Validate definitions before insertion and validate node results at runtime.
- Add or update focused unit tests for every new feature, including failure and boundary behavior. Add a Postgres integration test when behavior depends on transactions, leases, or tenant isolation. Do not use a passing compile check as a substitute for a behavior test.
- Update the relevant page in `docs/` when an API, flow contract, operational behavior, or security boundary changes. Keep `README.md` as the concise entry point.
- Add a new SQLx migration for database changes. Never edit an already applied migration.

## Verification

Run these before considering a change complete:

```sh
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --locked --all-targets -- -D warnings
```

The Postgres integration test requires `DATABASE_URL`; CI provides one. State clearly when it could not run locally. Regenerate `schemas/catalog.schema.json` with `cargo run --example print_schema > schemas/catalog.schema.json` whenever a flow or node schema changes.

## Documentation map

- `docs/architecture.md`: services, persistence, and event flow
- `docs/flows.md`: flow JSON contract and node catalog
- `docs/api.md`: authentication and HTTP endpoints
- `docs/operations.md`: local setup, migrations, and worker operation
- `docs/testing.md`: test coverage and commands
