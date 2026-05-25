#!/usr/bin/env python3
"""Research data gathering script for the research brief generator demo.

Executed by the Python IPC executor. Uses Vault-injected API keys to
simulate fetching data from external research sources.

Input (via IPC 'inputs' dict):
    request.json: {
        "topic": "...",
        "questions": ["...", ...],
        "sources": "...",
        "priority": "..."
    }

Environment (Vault-injected):
    API_KEY         — External research API key
    DATA_SOURCE_TOKEN — Data source access token

Output (via set_output):
    result: {
        "topic": "...",
        "findings": [...],
        "data_points": [...],
        "sources_consulted": [...],
        "raw_data_file": "raw_data.json"
    }
"""

import json
import os
import hashlib
import time

# ---------------------------------------------------------------------------
# Simulated research data gathering
# ---------------------------------------------------------------------------

RESEARCH_DATABASES = {
    "arxiv": {
        "name": "arXiv Preprints",
        "specialties": ["technology", "science", "ai", "machine learning"],
    },
    "pubmed": {
        "name": "PubMed Central",
        "specialties": ["health", "medicine", "biology", "biotech"],
    },
    "ssrn": {
        "name": "SSRN Working Papers",
        "specialties": ["economics", "finance", "policy", "regulation"],
    },
    "semantic_scholar": {
        "name": "Semantic Scholar",
        "specialties": ["general", "cross-domain"],
    },
}


def select_databases(topic):
    """Select relevant databases based on topic keywords."""
    topic_lower = topic.lower()
    selected = []
    for db_id, db_info in RESEARCH_DATABASES.items():
        for specialty in db_info["specialties"]:
            if specialty in topic_lower or specialty == "general":
                selected.append(db_id)
                break
    return selected if selected else ["semantic_scholar"]


def simulate_api_call(db_id, query, api_key, token):
    """Simulate an API call to a research database using injected credentials."""
    # Verify credentials are present (they come from Vault)
    if not api_key or not token:
        raise RuntimeError("Missing API credentials — Vault injection may have failed")

    # Simulate a deterministic but realistic response based on query hash
    query_hash = hashlib.sha256(f"{db_id}:{query}:{api_key[:8]}".encode()).hexdigest()[:8]
    db_name = RESEARCH_DATABASES[db_id]["name"]

    return {
        "source": db_name,
        "source_id": db_id,
        "query": query,
        "result_count": int(query_hash[:2], 16) % 50 + 5,
        "top_results": [
            {
                "title": f"[{db_name}] Analysis of {query} — finding {i+1}",
                "relevance": round(0.95 - i * 0.08, 2),
                "year": 2024 + (i % 2),
                "abstract": f"This study examines {query} with focus on recent developments. "
                f"Key insight #{i+1} from {db_name} corpus.",
            }
            for i in range(min(3, int(query_hash[:1], 16) % 4 + 1))
        ],
        "auth_verified": True,
        "request_id": query_hash,
    }


def gather_research(request):
    """Main research gathering logic."""
    topic = request.get("topic", "general research")
    raw_questions = request.get("questions", [])
    # Human UI textarea returns a newline-separated string; normalize to list
    if isinstance(raw_questions, str):
        questions = [q.strip() for q in raw_questions.splitlines() if q.strip()]
    else:
        questions = raw_questions
    sources_hint = request.get("sources", "")

    api_key = os.environ.get("API_KEY", "")
    data_source_token = os.environ.get("DATA_SOURCE_TOKEN", "")

    log_info(
        "Starting research data gathering",
        topic=topic,
        question_count=str(len(questions)),
    )

    # Select databases
    databases = select_databases(topic)
    log_info("Selected databases", databases=",".join(databases))

    # Gather data from each database for each question
    findings = []
    data_points = []
    sources_consulted = []

    for db_id in databases:
        # Query the topic first
        topic_results = simulate_api_call(db_id, topic, api_key, data_source_token)
        sources_consulted.append(topic_results["source"])

        for result in topic_results["top_results"]:
            findings.append({
                "source": topic_results["source"],
                "title": result["title"],
                "relevance": result["relevance"],
                "year": result["year"],
                "summary": result["abstract"],
            })

        # Then query each specific question
        for i, question in enumerate(questions):
            q_results = simulate_api_call(db_id, question, api_key, data_source_token)
            for result in q_results["top_results"]:
                data_points.append({
                    "question_index": i,
                    "question": question,
                    "source": q_results["source"],
                    "title": result["title"],
                    "relevance": result["relevance"],
                    "insight": result["abstract"],
                })

        log_info(f"Gathered data from {topic_results['source']}", results=str(len(topic_results["top_results"])))

    # Sort findings by relevance
    findings.sort(key=lambda f: f["relevance"], reverse=True)
    data_points.sort(key=lambda d: d["relevance"], reverse=True)

    # Build result
    result = {
        "topic": topic,
        "findings": findings[:10],  # top 10
        "data_points": data_points[:15],  # top 15
        "sources_consulted": list(set(sources_consulted)),
        "total_results_scanned": sum(
            simulate_api_call(db, topic, api_key, data_source_token)["result_count"]
            for db in databases
        ),
        "credentials_verified": bool(api_key and data_source_token),
        "gathered_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }

    # Write raw data to working directory
    raw_data_path = "raw_data.json"
    with open(raw_data_path, "w") as f:
        json.dump(result, f, indent=2)

    # Also write to the file-ops source storage directory so the file_ops
    # backend can copy it to the processed directory in the next step.
    source_dir = "/tmp/research/raw"
    os.makedirs(source_dir, exist_ok=True)
    with open(os.path.join(source_dir, "raw_data.json"), "w") as f:
        json.dump(result, f, indent=2)

    result["raw_data_file"] = raw_data_path
    return result


# ---------------------------------------------------------------------------
# Entry point — uses SDK-injected globals
# ---------------------------------------------------------------------------

request_data = inputs.get("request.json", {})
# `result` matches the orchestrator-declared output port; the runner's
# post-exec sweep promotes it into the executor's terminal status. The
# explicit `set_output` would be redundant.
result = gather_research(request_data)
log_info(
    "Research gathering complete",
    topic=result["topic"],
    findings=str(len(result["findings"])),
    data_points=str(len(result["data_points"])),
    credentials_ok=str(result["credentials_verified"]),
)
