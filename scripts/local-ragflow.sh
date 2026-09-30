#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
upstream_dir="$repo_root/third_party/ragflow"
export CHATFLOW_RAGFLOW_NETWORK="${CHATFLOW_RAGFLOW_NETWORK:-${RAGFLOW_COMPOSE_PROJECT:-chatflow-ragflow}-link}"
if [[ ! -f "$upstream_dir/docker/docker-compose.yml" || ! -f "$upstream_dir/docker/.env.example" ]]; then
  echo "Pinned RAGFlow Docker configuration is missing from third_party/ragflow" >&2
  exit 1
fi
if [[ ! -f "$upstream_dir/docker/.env" ]]; then
  cp "$upstream_dir/docker/.env.example" "$upstream_dir/docker/.env"
  chmod 600 "$upstream_dir/docker/.env"
fi

export MEM_LIMIT="${MEM_LIMIT:-4294967296}"
export TEI_MODEL="${TEI_MODEL:-BAAI/bge-small-en-v1.5}"
export COMPOSE_PROFILES="${COMPOSE_PROFILES:-${DOC_ENGINE:-elasticsearch},${DEVICE:-cpu},metadata-${METADATA_DB_PROFILE:-mysql},tei-cpu}"

if [[ "${1:-up}" == up ]]; then
  docker network inspect "$CHATFLOW_RAGFLOW_NETWORK" >/dev/null 2>&1 || \
    docker network create "$CHATFLOW_RAGFLOW_NETWORK" >/dev/null
fi

cd "$upstream_dir/docker"
if [[ $# -eq 0 ]]; then
  set -- up -d
fi
docker compose --project-name "${RAGFLOW_COMPOSE_PROJECT:-chatflow-ragflow}" \
  -f docker-compose.yml -f "$repo_root/ops/ragflow-local.override.yml" "$@"
