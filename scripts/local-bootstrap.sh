#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: scripts/local-bootstrap.sh TENANT_NAME" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
mkdir -p .local
umask 077
export DATABASE_URL="${DATABASE_URL:-postgres://chatflow:chatflow@127.0.0.1:5432/chatflow}"
bootstrap_output="$(cargo run --quiet --example bootstrap -- "$1")"
tenant_id="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^tenant_id: //p')"
api_key="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^api_key: //p')"
rag_config_key="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^rag_config_key: //p')"
ops_api_key="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^ops_api_key: //p')"
if [[ -z "$tenant_id" || "$api_key" != cf_* || "$rag_config_key" != cf_* || "$ops_api_key" != cf_* ]]; then
  echo "bootstrap output was incomplete" >&2
  exit 1
fi
printf '%s' "$ops_api_key" > .local/ops-api-key
printf '%s' "$rag_config_key" > .local/rag-config-key
printf '%s' "$api_key" > .local/api-key
echo "tenant_id: $tenant_id"
echo "Application, RAGFlow configuration, and Prometheus keys saved in .local/"
