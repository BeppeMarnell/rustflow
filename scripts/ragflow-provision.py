"""Run inside the pinned official RAGFlow CPU container after its services start."""

import io
import json
import os
import sys
import time

import requests

sys.path.insert(0, "/ragflow")
from api.utils.crypt import crypt  # noqa: E402

ADMIN = "http://127.0.0.1:9381/api/v1/admin"
API = "http://127.0.0.1:9380/api/v1"
EMAIL = os.environ.get("DEFAULT_SUPERUSER_EMAIL", "admin@ragflow.io")
PASSWORD = os.environ.get("ADMIN_DEFAULT_PASSWORD") or os.environ.get("DEFAULT_SUPERUSER_PASSWORD")
DATASET_NAME = os.environ.get("RAGFLOW_DATASET_NAME", "chatflow-local")
EMBEDDING = "BAAI/bge-small-en-v1.5@default@Builtin"
DOCUMENT = b'Chatflow local RAGFlow verification. The verification phrase is "cedar moon 417".\n'
if not PASSWORD:
    raise SystemExit("RAGFlow admin bootstrap password is unavailable")

session = requests.Session()


def result(method, url, **kwargs):
    response = session.request(method, url, timeout=45, **kwargs)
    response.raise_for_status()
    value = response.json()
    if value.get("code") != 0:
        raise RuntimeError(f"RAGFlow returned an error: {value.get('message')}")
    return value["data"]


for attempt in range(120):
    try:
        result("GET", ADMIN + "/ping")
        session.get(API + "/system/ping", timeout=5).raise_for_status()
        break
    except (requests.RequestException, RuntimeError):
        time.sleep(2)
else:
    raise SystemExit("RAGFlow API and admin service did not become ready")

login = session.post(
    ADMIN + "/login", json={"email": EMAIL, "password": crypt(PASSWORD)}, timeout=45
)
login.raise_for_status()
if login.json().get("code") != 0:
    raise SystemExit("RAGFlow admin login failed")
admin_token = login.headers["Authorization"]
key_data = result(
    "POST", ADMIN + f"/users/{EMAIL}/keys", headers={"Authorization": admin_token}
)
api_key = key_data["token"]
assert api_key.startswith("ragflow-")
headers = {"Authorization": f"Bearer {api_key}"}

datasets = result("GET", API + "/datasets", headers=headers)
matching = [dataset for dataset in datasets if dataset["name"] == DATASET_NAME]
if matching:
    dataset_id = matching[0]["id"]
    if matching[0].get("embedding_model") != EMBEDDING:
        raise RuntimeError("existing RAGFlow dataset uses a different embedding model")
else:
    dataset = result(
        "POST",
        API + "/datasets",
        headers=headers,
        json={"name": DATASET_NAME, "embedding_model": EMBEDDING, "chunk_method": "naive"},
    )
    dataset_id = dataset["id"]

docs = result("GET", API + f"/datasets/{dataset_id}/documents", headers=headers)["docs"]
matching_docs = [doc for doc in docs if doc["name"] == "ragflow-smoke.txt"]
if matching_docs:
    document_id = matching_docs[0]["id"]
else:
    uploaded = result(
        "POST",
        API + f"/datasets/{dataset_id}/documents",
        headers=headers,
        files={"file": ("ragflow-smoke.txt", io.BytesIO(DOCUMENT), "text/plain")},
    )
    document_id = uploaded[0]["id"]

if not matching_docs or matching_docs[0]["run"] != "DONE":
    result(
        "POST",
        API + f"/datasets/{dataset_id}/documents/parse",
        headers=headers,
        json={"document_ids": [document_id]},
    )
    for attempt in range(180):
        docs = result("GET", API + f"/datasets/{dataset_id}/documents", headers=headers)["docs"]
        current = next(doc for doc in docs if doc["id"] == document_id)
        if current["run"] == "DONE" and current.get("chunk_count", 0) > 0:
            break
        if current["run"] == "FAIL":
            raise RuntimeError("RAGFlow document parsing failed")
        time.sleep(2)
    else:
        raise RuntimeError("RAGFlow document parsing timed out")

retrieval = result(
    "POST",
    API + "/retrieval",
    headers=headers,
    json={"question": "What is the verification phrase?", "dataset_ids": [dataset_id], "top_k": 3},
)
chunks = retrieval.get("chunks", [])
if not any("cedar moon 417" in chunk.get("content", "") for chunk in chunks):
    raise RuntimeError("RAGFlow retrieval did not return the verification phrase")
print(json.dumps({"api_key": api_key, "dataset_id": dataset_id, "document_id": document_id, "retrieval_chunks": len(chunks)}))
