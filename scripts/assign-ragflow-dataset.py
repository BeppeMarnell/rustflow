#!/usr/bin/env python3
"""Bind the provisioned RAGFlow dataset to the newly bootstrapped Chatflow tenant."""

import json
import os
from pathlib import Path
import urllib.request

base = os.environ["CHATFLOW_API_URL"].rstrip("/")
state = Path(os.environ["CHATFLOW_STATE_DIR"])
dataset_id = (state / "ragflow-dataset-id").read_text().strip()
key = (state / "rag-config-key").read_text().strip()
body = json.dumps({"dataset_ids": [dataset_id]}).encode()
request = urllib.request.Request(
    base + "/v1/ragflow-datasets",
    data=body,
    headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    method="PUT",
)
with urllib.request.urlopen(request, timeout=10) as response:
    assigned = json.load(response)
if assigned.get("dataset_ids") != [dataset_id]:
    raise SystemExit("Chatflow did not store the provisioned RAGFlow dataset")
print(f"RAGFlow dataset assigned: {dataset_id}")
