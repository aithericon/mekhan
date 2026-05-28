#!/usr/bin/env python3
"""Invoice validation script for the invoice processing pipeline demo.

Executed by the Python IPC executor. Validates OCR-extracted invoice data,
checks totals, flags anomalies, and performs vendor verification using
Vault-injected credentials.

Input (the workflow token, via `aithericon.token()`):
    {
        "vendor": "...",
        "invoice_number": "...",
        "date": "...",
        "line_items": [{"description", "quantity", "unit_price", "amount"}, ...],
        "subtotal": ...,
        "tax": ...,
        "total": ...,
        "payment_terms": "...",
        "department": "...",
        "urgency": "..."
    }

Environment (Vault-injected):
    VALIDATION_API_KEY — External validation API key

Output (via set_output):
    result: {
        "validated_data": {...},
        "risk_flags": [...],
        "compliance_status": "compliant" | "review_required" | "non_compliant",
        "anomalies": [...],
        "validation_score": 0.0-1.0
    }
"""

import json
import os
import hashlib

# ---------------------------------------------------------------------------
# Mock vendor database
# ---------------------------------------------------------------------------

KNOWN_VENDORS = {
    "acme corporation": {
        "status": "approved",
        "tax_id": "XX-1234567",
        "risk": "low",
        "category": "technology",
    },
    "acme corp": {
        "status": "approved",
        "tax_id": "XX-1234567",
        "risk": "low",
        "category": "technology",
    },
    "globex industries": {
        "status": "approved",
        "tax_id": "XX-7654321",
        "risk": "medium",
        "category": "manufacturing",
    },
    "initech solutions": {
        "status": "approved",
        "tax_id": "XX-9876543",
        "risk": "low",
        "category": "consulting",
    },
    "umbrella corp": {
        "status": "flagged",
        "tax_id": "XX-0000000",
        "risk": "high",
        "category": "biotech",
    },
}

# Policy rules
POLICY = {
    "max_single_item": 10000.0,
    "max_invoice_total": 50000.0,
    "tax_rate_min": 0.0,
    "tax_rate_max": 0.15,
    "required_fields": ["vendor", "invoice_number", "date", "total"],
    "suspicious_amounts": [999.99, 9999.99, 4999.99],
}


def validate_required_fields(data):
    """Check that all required fields are present and non-empty."""
    missing = []
    for field in POLICY["required_fields"]:
        val = data.get(field)
        if val is None or (isinstance(val, str) and not val.strip()):
            missing.append(field)
    return missing


def validate_totals(data):
    """Verify line item totals match subtotal and total."""
    anomalies = []
    line_items = data.get("line_items", [])

    if not line_items:
        anomalies.append("No line items found in invoice")
        return anomalies

    # Sum line item amounts
    computed_subtotal = 0.0
    for i, item in enumerate(line_items):
        amount = float(item.get("amount") or 0)
        qty = float(item.get("quantity") or 0)
        unit_price = float(item.get("unit_price") or 0)

        # Check quantity * unit_price ≈ amount
        expected = round(qty * unit_price, 2)
        if abs(expected - amount) > 0.02:
            anomalies.append(
                f"Line item {i+1} ({item.get('description', '?')}): "
                f"qty({qty}) x price({unit_price}) = {expected}, but amount is {amount}"
            )

        computed_subtotal += amount

    computed_subtotal = round(computed_subtotal, 2)
    stated_subtotal = float(data.get("subtotal") or 0)

    if abs(computed_subtotal - stated_subtotal) > 0.02:
        anomalies.append(
            f"Subtotal mismatch: line items sum to {computed_subtotal}, "
            f"but stated subtotal is {stated_subtotal}"
        )

    # Check subtotal + tax ≈ total
    tax = float(data.get("tax") or 0)
    total = float(data.get("total") or 0)
    expected_total = round(stated_subtotal + tax, 2)

    if abs(expected_total - total) > 0.02:
        anomalies.append(
            f"Total mismatch: subtotal({stated_subtotal}) + tax({tax}) = {expected_total}, "
            f"but stated total is {total}"
        )

    return anomalies


def check_tax_rate(data):
    """Check if the tax rate is within acceptable range."""
    subtotal = float(data.get("subtotal", 0))
    tax = float(data.get("tax", 0))

    if subtotal <= 0:
        return None

    rate = tax / subtotal
    if rate < POLICY["tax_rate_min"] or rate > POLICY["tax_rate_max"]:
        return f"Tax rate {rate:.2%} outside expected range ({POLICY['tax_rate_min']:.0%}-{POLICY['tax_rate_max']:.0%})"
    return None


def check_vendor(vendor_name, api_key):
    """Verify vendor against known database using Vault-injected credentials."""
    if not api_key:
        return {"status": "unverified", "reason": "Missing API key"}

    # Simulate API call with credential verification
    key_hash = hashlib.sha256(api_key.encode()).hexdigest()[:8]

    vendor_lower = vendor_name.lower().strip()
    vendor_info = KNOWN_VENDORS.get(vendor_lower)

    if vendor_info:
        return {
            "status": vendor_info["status"],
            "risk": vendor_info["risk"],
            "category": vendor_info["category"],
            "verified": True,
            "verification_id": key_hash,
        }
    else:
        return {
            "status": "unknown",
            "risk": "medium",
            "category": "unclassified",
            "verified": False,
            "verification_id": key_hash,
        }


def check_policy_limits(data):
    """Check invoice amounts against policy limits."""
    flags = []
    total = float(data.get("total", 0))

    if total > POLICY["max_invoice_total"]:
        flags.append(f"Invoice total ${total:,.2f} exceeds max ${POLICY['max_invoice_total']:,.2f}")

    for i, item in enumerate(data.get("line_items", [])):
        amount = float(item.get("amount", 0))
        if amount > POLICY["max_single_item"]:
            flags.append(
                f"Line item {i+1} ({item.get('description', '?')}) "
                f"amount ${amount:,.2f} exceeds single-item max ${POLICY['max_single_item']:,.2f}"
            )
        # Check for suspicious round amounts
        for sus in POLICY["suspicious_amounts"]:
            if abs(amount - sus) < 0.01:
                flags.append(
                    f"Line item {i+1} suspicious round amount: ${amount:,.2f}"
                )

    return flags


def compute_score(anomalies, risk_flags, vendor_result):
    """Compute a validation score from 0.0 to 1.0."""
    score = 1.0

    # Deductions for anomalies
    score -= len(anomalies) * 0.15

    # Deductions for risk flags
    score -= len(risk_flags) * 0.10

    # Vendor risk deductions
    vendor_risk = vendor_result.get("risk", "medium")
    if vendor_risk == "high":
        score -= 0.25
    elif vendor_risk == "medium":
        score -= 0.10
    if not vendor_result.get("verified", False):
        score -= 0.15

    return max(0.0, min(1.0, round(score, 2)))


def assess_compliance(score, anomalies, risk_flags, vendor_result):
    """Determine compliance status."""
    if score >= 0.8 and not anomalies and vendor_result.get("status") == "approved":
        return "compliant"
    elif score < 0.5 or vendor_result.get("status") == "flagged":
        return "non_compliant"
    else:
        return "review_required"


def validate_invoice(data):
    """Main validation logic."""
    anomalies = []
    risk_flags = []

    api_key = os.environ.get("VALIDATION_API_KEY", "")

    log_info(
        "Starting invoice validation",
        vendor=data.get("vendor", "unknown"),
        invoice=data.get("invoice_number", "unknown"),
    )

    # 1. Required fields
    missing = validate_required_fields(data)
    if missing:
        anomalies.append(f"Missing required fields: {', '.join(missing)}")

    # 2. Validate totals
    total_anomalies = validate_totals(data)
    anomalies.extend(total_anomalies)

    # 3. Check tax rate
    tax_issue = check_tax_rate(data)
    if tax_issue:
        risk_flags.append(tax_issue)

    # 4. Vendor verification
    vendor_result = check_vendor(data.get("vendor", ""), api_key)
    if vendor_result["status"] == "flagged":
        risk_flags.append(f"Vendor '{data.get('vendor')}' is FLAGGED in database")
    elif vendor_result["status"] == "unknown":
        risk_flags.append(f"Vendor '{data.get('vendor')}' not found in approved vendor list")

    # 5. Policy limits
    policy_flags = check_policy_limits(data)
    risk_flags.extend(policy_flags)

    # 6. Urgency check
    if data.get("urgency") == "critical":
        risk_flags.append("Invoice marked as CRITICAL urgency — expedited review required")

    # 7. Compute validation score
    score = compute_score(anomalies, risk_flags, vendor_result)

    # 8. Assess compliance
    compliance = assess_compliance(score, anomalies, risk_flags, vendor_result)

    log_info(
        "Validation complete",
        score=str(score),
        anomalies=str(len(anomalies)),
        risk_flags=str(len(risk_flags)),
        compliance=compliance,
    )

    # Write validated data to outputs dir (uploaded to S3 via per-output upload_to)
    outputs_dir = os.environ.get("AITHERICON_OUTPUTS_DIR", "/tmp/invoice_processing/validated")
    os.makedirs(outputs_dir, exist_ok=True)
    with open(os.path.join(outputs_dir, "validated_invoice.json"), "w") as f:
        json.dump({
            "invoice_data": data,
            "validation": {
                "score": score,
                "anomalies": anomalies,
                "risk_flags": risk_flags,
                "compliance_status": compliance,
                "vendor_verification": vendor_result,
            },
        }, f, indent=2)

    return {
        "validated_data": data,
        "risk_flags": risk_flags,
        "compliance_status": compliance,
        "anomalies": anomalies,
        "validation_score": score,
        "vendor_verification": vendor_result,
    }


# ---------------------------------------------------------------------------
# Entry point — reads the accumulating workflow token; `set_output` / `log_*`
# are SDK-injected globals (the runner auto-imports the SDK).
# ---------------------------------------------------------------------------

import aithericon

invoice_data = aithericon.token()
# `result` matches the orchestrator-declared output port; the runner's
# post-exec sweep promotes it into the executor's terminal status.
result = validate_invoice(invoice_data)
log_info(
    "Invoice validation complete",
    invoice=invoice_data.get("invoice_number", "unknown"),
    score=str(result["validation_score"]),
    compliance=result["compliance_status"],
)
