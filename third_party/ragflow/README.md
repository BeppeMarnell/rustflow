# RAGFlow Docker configuration

These files are copied unchanged from the official InfiniFlow RAGFlow repository,
tag `v0.27.2`, commit `a024bea0cd93f39e6652a42bf84dd20c55bc560b`.
They are used by `scripts/local-ragflow.sh` to run the official published
RAGFlow and text-embeddings-inference images. The root repository's
`ops/ragflow-local.override.yml` supplies loopback port bindings.
Keep `docker/.env` when copying the repository; Compose reads these upstream
defaults even though the file is hidden.

RAGFlow is licensed under Apache License 2.0; see `LICENSE`. The vendored files
are configuration and launch scripts only. Chatflow does not implement RAGFlow.
