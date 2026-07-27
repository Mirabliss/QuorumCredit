#!/usr/bin/env python3
"""Monitor QuorumCredit contract state for anomalies and generate daily reports.

Unlike scripts/consensus_monitor.py (which compares state *across*
validators), this script tracks state *over time* on a single trusted
RPC/indexer endpoint: total loans, average interest rate, active vouches,
default rate, storage usage, and gas cost trends, alerting when any
metric moves outside its expected band.

Usage:
    ./scripts/state_anomaly_monitor.py --config scripts/state_anomaly_monitor.config.json
    ./scripts/state_anomaly_monitor.py --config ... --report-only   # just print the daily report

Exit codes:
    0  no anomalies
    1  anomaly detected and alert sent (or would be, in --dry-run)
    2  configuration or connectivity error
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.request
import urllib.error
from dataclasses import dataclass, asdict


@dataclass
class StateSnapshot:
    captured_at: str
    total_loans: int
    active_loans: int
    defaulted_loans: int
    repaid_loans: int
    average_interest_rate_bps: float
    active_vouches: int
    total_staked: int
    storage_entries: int
    storage_limit: int
    avg_gas_per_tx: float


def rpc_call(endpoint: str, method: str, params: dict, timeout: float = 15.0) -> dict:
    payload = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    ).encode("utf-8")
    req = urllib.request.Request(
        endpoint, data=payload, headers={"Content-Type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def fetch_snapshot(config: dict) -> StateSnapshot:
    """Fetch current state via the indexer's metrics/summary endpoint.

    Expects the QuorumCredit indexer (see docs/monitoring-setup-guide.md)
    to expose a JSON summary endpoint. Adjust `indexer_summary_url` in the
    config if your deployment exposes this differently (e.g. scraping
    Prometheus directly instead).
    """
    url = config["indexer_summary_url"]
    req = urllib.request.Request(url)
    with urllib.request.urlopen(req, timeout=15.0) as resp:
        data = json.loads(resp.read().decode("utf-8"))

    return StateSnapshot(
        captured_at=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        total_loans=data.get("total_loans", 0),
        active_loans=data.get("active_loans", 0),
        defaulted_loans=data.get("defaulted_loans", 0),
        repaid_loans=data.get("repaid_loans", 0),
        average_interest_rate_bps=data.get("average_interest_rate_bps", 0.0),
        active_vouches=data.get("active_vouches", 0),
        total_staked=data.get("total_staked", 0),
        storage_entries=data.get("storage_entries", 0),
        storage_limit=data.get("storage_limit", config.get("default_storage_limit", 0)),
        avg_gas_per_tx=data.get("avg_gas_per_tx", 0.0),
    )


def load_history(history_path: str) -> list[dict]:
    if not os.path.exists(history_path):
        return []
    with open(history_path) as f:
        return json.load(f)


def save_history(history_path: str, history: list[dict], max_entries: int) -> None:
    trimmed = history[-max_entries:]
    with open(history_path, "w") as f:
        json.dump(trimmed, f, indent=2)


def detect_anomalies(
    snapshot: StateSnapshot, history: list[dict], thresholds: dict
) -> list[str]:
    anomalies: list[str] = []

    if not history:
        return anomalies  # no baseline yet

    previous = history[-1]

    # Sudden increase in defaults
    prev_defaults = previous.get("defaulted_loans", 0)
    default_delta = snapshot.defaulted_loans - prev_defaults
    max_default_delta = thresholds.get("max_default_increase_per_poll", 5)
    if default_delta > max_default_delta:
        anomalies.append(
            f"defaulted_loans jumped by {default_delta} since last poll "
            f"(threshold {max_default_delta}) — {prev_defaults} -> {snapshot.defaulted_loans}"
        )

    # Unusual loan sizes (via average interest rate as a proxy signal, plus
    # total_loans growth rate)
    prev_total = previous.get("total_loans", 0)
    loan_growth = snapshot.total_loans - prev_total
    max_loan_growth = thresholds.get("max_loan_count_increase_per_poll", 1000)
    if loan_growth > max_loan_growth:
        anomalies.append(
            f"total_loans grew by {loan_growth} since last poll "
            f"(threshold {max_loan_growth}) — possible spam or exploit"
        )

    rate_min = thresholds.get("interest_rate_bps_min", 0)
    rate_max = thresholds.get("interest_rate_bps_max", 5000)
    if not (rate_min <= snapshot.average_interest_rate_bps <= rate_max):
        anomalies.append(
            f"average_interest_rate_bps {snapshot.average_interest_rate_bps} is outside "
            f"expected range [{rate_min}, {rate_max}]"
        )

    # Storage usage approaching limits
    if snapshot.storage_limit > 0:
        usage_pct = snapshot.storage_entries / snapshot.storage_limit * 100
        warn_pct = thresholds.get("storage_usage_warn_pct", 80)
        if usage_pct >= warn_pct:
            anomalies.append(
                f"storage usage at {usage_pct:.1f}% of limit "
                f"({snapshot.storage_entries}/{snapshot.storage_limit}), threshold {warn_pct}%"
            )

    # Gas cost trend
    prev_gas = previous.get("avg_gas_per_tx", 0.0)
    if prev_gas > 0:
        gas_growth_pct = (snapshot.avg_gas_per_tx - prev_gas) / prev_gas * 100
        max_gas_growth_pct = thresholds.get("max_gas_growth_pct", 25)
        if gas_growth_pct > max_gas_growth_pct:
            anomalies.append(
                f"avg_gas_per_tx increased {gas_growth_pct:.1f}% since last poll "
                f"({prev_gas:.0f} -> {snapshot.avg_gas_per_tx:.0f}), threshold {max_gas_growth_pct}%"
            )

    # Active vouches collapsing (possible mass-withdrawal event)
    prev_vouches = previous.get("active_vouches", 0)
    if prev_vouches > 0:
        vouch_drop_pct = (prev_vouches - snapshot.active_vouches) / prev_vouches * 100
        max_vouch_drop_pct = thresholds.get("max_vouch_drop_pct", 30)
        if vouch_drop_pct > max_vouch_drop_pct:
            anomalies.append(
                f"active_vouches dropped {vouch_drop_pct:.1f}% since last poll "
                f"({prev_vouches} -> {snapshot.active_vouches}), threshold {max_vouch_drop_pct}%"
            )

    return anomalies


def send_alert(message: str, config: dict, dry_run: bool) -> None:
    webhook = config.get("alert_webhook")
    prefix = "[DRY RUN] " if dry_run else ""
    print(f"{prefix}ANOMALY ALERT: {message}", file=sys.stderr)
    if webhook and not dry_run:
        try:
            payload = json.dumps(
                {"text": f"QuorumCredit state anomaly: {message}"}
            ).encode()
            req = urllib.request.Request(
                webhook, data=payload, headers={"Content-Type": "application/json"}
            )
            urllib.request.urlopen(req, timeout=10)
        except (urllib.error.URLError, TimeoutError) as exc:
            print(f"Failed to deliver alert webhook: {exc}", file=sys.stderr)


def generate_daily_report(snapshot: StateSnapshot, history: list[dict]) -> str:
    import calendar

    day_ago_cutoff = time.time() - 86400
    day_entries = [
        h
        for h in history
        if calendar.timegm(time.strptime(h["captured_at"], "%Y-%m-%dT%H:%M:%SZ")) >= day_ago_cutoff
    ]
    lines = [
        "# QuorumCredit Daily State Report",
        f"Generated: {snapshot.captured_at}",
        "",
        "## Current State",
        f"- Total loans: {snapshot.total_loans}",
        f"- Active loans: {snapshot.active_loans}",
        f"- Repaid loans: {snapshot.repaid_loans}",
        f"- Defaulted loans: {snapshot.defaulted_loans}",
        f"- Average interest rate: {snapshot.average_interest_rate_bps} bps",
        f"- Active vouches: {snapshot.active_vouches}",
        f"- Total staked: {snapshot.total_staked}",
        f"- Storage entries: {snapshot.storage_entries} / {snapshot.storage_limit}",
        f"- Avg gas per tx: {snapshot.avg_gas_per_tx:.0f}",
        "",
        f"## History Points Retained: {len(history) + 1} (this poll included, {len(day_entries)} in last 24h window)",
    ]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, help="Path to JSON config file")
    parser.add_argument("--dry-run", action="store_true", help="Don't actually send alerts")
    parser.add_argument(
        "--report-only", action="store_true", help="Print the daily report and exit, skip anomaly alerting"
    )
    args = parser.parse_args()

    try:
        with open(args.config) as f:
            config = json.load(f)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"Failed to load config: {exc}", file=sys.stderr)
        return 2

    history_path = config.get("history_path", "artifacts/state_history.json")
    max_history = config.get("max_history_entries", 720)  # ~30 days at hourly polls
    thresholds = config.get("thresholds", {})

    try:
        snapshot = fetch_snapshot(config)
    except (urllib.error.URLError, TimeoutError, KeyError, ValueError) as exc:
        print(f"Failed to fetch state snapshot: {exc}", file=sys.stderr)
        return 2

    history = load_history(history_path)

    if args.report_only:
        print(generate_daily_report(snapshot, history))
        return 0

    anomalies = detect_anomalies(snapshot, history, thresholds)

    os.makedirs(os.path.dirname(history_path) or ".", exist_ok=True)
    history.append(asdict(snapshot))
    save_history(history_path, history, max_history)

    report = {
        "snapshot": asdict(snapshot),
        "anomalies": anomalies,
        "healthy": len(anomalies) == 0,
    }
    print(json.dumps(report, indent=2))

    if anomalies:
        send_alert("; ".join(anomalies), config, args.dry_run)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
