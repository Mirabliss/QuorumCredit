#!/usr/bin/env python3
"""Monitor contract state consistency across multiple Soroban RPC validators.

QuorumCredit's contract state should be identical when read from any
healthy validator/RPC node. This script polls a configurable list of
RPC endpoints, compares a set of read-only contract views across them,
alerts on any mismatch, and can optionally attempt a bounded
reconciliation read-retry before escalating.

Usage:
    ./scripts/consensus_monitor.py --config scripts/consensus_monitor.config.json

Exit codes:
    0  all validators agree
    1  divergence detected and alert was sent (or would be, in --dry-run)
    2  configuration or connectivity error
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request
import urllib.error
from dataclasses import dataclass, field
from typing import Any


DEFAULT_CHECKS = [
    "get_config",
    "get_fee_treasury",
    "get_slash_treasury",
]


@dataclass
class ValidatorResult:
    name: str
    endpoint: str
    ledger: int | None = None
    values: dict[str, Any] = field(default_factory=dict)
    error: str | None = None


def rpc_call(endpoint: str, method: str, params: dict, timeout: float = 10.0) -> dict:
    payload = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    ).encode("utf-8")
    req = urllib.request.Request(
        endpoint, data=payload, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def get_latest_ledger(endpoint: str) -> int:
    result = rpc_call(endpoint, "getLatestLedger", {})
    return int(result.get("result", {}).get("sequence", -1))


def simulate_contract_call(
    endpoint: str, contract_id: str, function_name: str
) -> Any:
    """Simulate a read-only contract invocation via simulateTransaction.

    In production this should build a proper Soroban invoke-host-function
    transaction envelope with the Stellar SDK. This lightweight version
    calls a simplified RPC method name so the monitor can run without a
    heavyweight SDK dependency; swap in the real transaction-building
    logic for your deployment (see docs/consensus-monitoring-guide.md).
    """
    result = rpc_call(
        endpoint,
        "simulateTransaction",
        {"contractId": contract_id, "function": function_name},
    )
    return result.get("result")


def poll_validator(
    name: str, endpoint: str, contract_id: str, checks: list[str]
) -> ValidatorResult:
    result = ValidatorResult(name=name, endpoint=endpoint)
    try:
        result.ledger = get_latest_ledger(endpoint)
        for check in checks:
            result.values[check] = simulate_contract_call(endpoint, contract_id, check)
    except (urllib.error.URLError, TimeoutError, ValueError) as exc:
        result.error = str(exc)
    return result


def compare_results(
    results: list[ValidatorResult], ledger_tolerance: int
) -> list[str]:
    """Return a list of human-readable divergence messages, empty if none."""
    problems: list[str] = []

    healthy = [r for r in results if r.error is None]
    for r in results:
        if r.error is not None:
            problems.append(f"validator '{r.name}' ({r.endpoint}) unreachable: {r.error}")

    if len(healthy) < 2:
        return problems  # nothing to compare

    ledgers = {r.name: r.ledger for r in healthy if r.ledger is not None}
    if ledgers:
        max_ledger = max(ledgers.values())
        for name, ledger in ledgers.items():
            if max_ledger - ledger > ledger_tolerance:
                problems.append(
                    f"validator '{name}' is {max_ledger - ledger} ledgers behind "
                    f"(at {ledger}, network at {max_ledger})"
                )

    baseline = healthy[0]
    for check in baseline.values:
        baseline_value = baseline.values.get(check)
        for other in healthy[1:]:
            other_value = other.values.get(check)
            if other_value != baseline_value:
                problems.append(
                    f"state mismatch on '{check}': '{baseline.name}'={baseline_value!r} "
                    f"vs '{other.name}'={other_value!r}"
                )

    return problems


def send_alert(message: str, config: dict, dry_run: bool) -> None:
    webhook = config.get("alert_webhook")
    prefix = "[DRY RUN] " if dry_run else ""
    print(f"{prefix}ALERT: {message}", file=sys.stderr)
    if webhook and not dry_run:
        try:
            payload = json.dumps({"text": f"QuorumCredit consensus alert: {message}"}).encode()
            req = urllib.request.Request(
                webhook, data=payload, headers={"Content-Type": "application/json"}
            )
            urllib.request.urlopen(req, timeout=10)
        except (urllib.error.URLError, TimeoutError) as exc:
            print(f"Failed to deliver alert webhook: {exc}", file=sys.stderr)


def reconcile(
    validators: list[dict], contract_id: str, checks: list[str], retries: int, delay: float
) -> list[ValidatorResult]:
    """Retry polling to distinguish transient lag from real divergence."""
    for attempt in range(1, retries + 1):
        results = [
            poll_validator(v["name"], v["endpoint"], contract_id, checks)
            for v in validators
        ]
        problems = compare_results(results, ledger_tolerance=2)
        if not problems:
            return results
        print(
            f"Reconciliation attempt {attempt}/{retries}: {len(problems)} issue(s) found, retrying...",
            file=sys.stderr,
        )
        time.sleep(delay)
    return results


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, help="Path to JSON config file")
    parser.add_argument("--dry-run", action="store_true", help="Don't actually send alerts")
    parser.add_argument(
        "--reconcile-retries",
        type=int,
        default=3,
        help="Number of re-poll attempts before treating a mismatch as confirmed",
    )
    parser.add_argument(
        "--reconcile-delay",
        type=float,
        default=5.0,
        help="Seconds to wait between reconciliation attempts",
    )
    args = parser.parse_args()

    try:
        with open(args.config) as f:
            config = json.load(f)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"Failed to load config: {exc}", file=sys.stderr)
        return 2

    validators = config.get("validators", [])
    contract_id = config.get("contract_id")
    checks = config.get("checks", DEFAULT_CHECKS)

    if not validators or not contract_id:
        print("Config must include 'validators' and 'contract_id'", file=sys.stderr)
        return 2

    results = reconcile(
        validators, contract_id, checks, args.reconcile_retries, args.reconcile_delay
    )
    problems = compare_results(results, ledger_tolerance=2)

    report = {
        "checked_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "validators": [
            {"name": r.name, "ledger": r.ledger, "error": r.error} for r in results
        ],
        "divergences": problems,
        "healthy": len(problems) == 0,
    }
    print(json.dumps(report, indent=2))

    if problems:
        send_alert("; ".join(problems), config, args.dry_run)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
