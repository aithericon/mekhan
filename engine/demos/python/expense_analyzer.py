#!/usr/bin/env python3
"""Expense analyzer for the expense approval workflow demo.

This script is executed by the Python IPC executor to analyze expense requests.
It evaluates policy compliance, assigns risk levels, and determines if
auto-approval is possible.

Input (via stdin as JSON):
    {
        "expense_id": "EXP-001",
        "amount": 45.00,
        "currency": "USD",
        "category": "meals",
        "description": "Team lunch",
        "submitter": "alice@example.com"
    }

Output (via stdout as JSON):
    {
        "risk_level": "low",
        "policy_violations": [],
        "suggested_category": "meals",
        "auto_approve": true
    }
"""

import json
import sys
from typing import Any

# Policy thresholds
POLICY = {
    "auto_approve_limit": 100.0,  # Auto-approve expenses under this amount
    "review_limit": 500.0,        # Medium risk: needs manager review
    "high_risk_limit": 1000.0,    # High risk: needs senior approval
    "valid_categories": ["meals", "travel", "equipment", "software", "training", "misc"],
    "suspicious_keywords": ["personal", "gift", "party", "alcohol"],
}


def analyze_expense(inputs: dict[str, Any]) -> dict[str, Any]:
    """Analyze an expense request for policy compliance and risk."""
    amount = float(inputs.get("amount", 0))
    category = inputs.get("category", "misc").lower()
    description = inputs.get("description", "").lower()

    violations = []
    risk_level = "low"
    auto_approve = True
    suggested_category = category

    # Check amount thresholds
    if amount >= POLICY["high_risk_limit"]:
        risk_level = "high"
        auto_approve = False
        violations.append(f"Amount ${amount} exceeds high-risk threshold (${POLICY['high_risk_limit']})")
    elif amount >= POLICY["review_limit"]:
        risk_level = "medium"
        auto_approve = False
        violations.append(f"Amount ${amount} requires manager review (>${POLICY['review_limit']})")
    elif amount >= POLICY["auto_approve_limit"]:
        risk_level = "low"
        auto_approve = False  # Still needs review but low risk

    # Check category validity
    if category not in POLICY["valid_categories"]:
        violations.append(f"Invalid category '{category}'")
        suggested_category = "misc"
        if risk_level == "low":
            risk_level = "medium"
        auto_approve = False

    # Check for suspicious keywords
    for keyword in POLICY["suspicious_keywords"]:
        if keyword in description:
            violations.append(f"Description contains suspicious keyword: '{keyword}'")
            if risk_level == "low":
                risk_level = "medium"
            auto_approve = False

    # Category-specific rules
    if category == "equipment" and amount > 200:
        violations.append("Equipment purchases over $200 require manager approval")
        auto_approve = False
        if risk_level == "low":
            risk_level = "medium"

    if category == "travel" and amount > 300:
        violations.append("Travel expenses over $300 require pre-approval")
        auto_approve = False

    return {
        "risk_level": risk_level,
        "policy_violations": violations,
        "suggested_category": suggested_category,
        "auto_approve": auto_approve,
    }


# ---------------------------------------------------------------------------
# Entry point - uses SDK-injected 'inputs' variable when run via executor
# ---------------------------------------------------------------------------

# The executor's Python runner injects: inputs, set_output, log_info, etc.
# 'inputs' is a dict where keys are input file names, values are parsed content.

# Get expense data from the staged input.json
expense_data = inputs.get("input.json", {})

# Run analysis. The orchestrator declares `outputs: [#{ name: "result" }]`
# on the dispatching effect, so the runner's post-exec sweep promotes the
# top-level `result` global into the executor's terminal status — no
# explicit `set_output` needed.
result = analyze_expense(expense_data)

# Also log for debugging
log_info(
    f"Expense {expense_data.get('expense_id', 'unknown')} analyzed",
    risk_level=result["risk_level"],
    auto_approve=str(result["auto_approve"]),
)
