# Progressive Deployment with Canary Releases (Issue #1231)

Server deploys were previously big-bang: a new version replaced the old one everywhere at once, with no automated way to catch a bad build before it affected all traffic. `scripts/canary_deploy.sh` adds a staged rollout with automatic rollback.

## What it does

1. (Optional) Runs `--deploy-cmd` to stand up the canary instance.
2. Steps through traffic percentages (default `5 25 50 100`):
   - Applies the weight (via a pluggable `--lb-adapter`, or prints it as an operator instruction if none is configured — see below).
   - Observes the canary's `/metrics` for `--window-seconds` (default 60s).
   - Computes the **error rate** (`qc_http_request_errors_total` / `qc_http_requests_total`, over the window) and **average latency** (`qc_http_request_duration_ms_sum` / `qc_http_request_duration_ms_count`, over the window) — both instrumented in `server/src/index.ts`'s HTTP request handler.
   - If `--stable-url` is given, also compares the canary's error rate against the stable fleet's over the same window.
3. **Automatic rollback**: if any step exceeds `--max-error-rate` (default 5%), `--max-latency-ms` (default 500ms), or the canary's error rate exceeds the stable fleet's by more than `--max-error-rate-multiplier` (default 2x), traffic is immediately shifted back to 0% and the script exits non-zero.
4. Every step's decision (promote/rollback, observed error rate, observed latency) is appended to `deploy/canary-history.jsonl`.

## Traffic application: the load-balancer adapter seam

This repo has no load balancer, ingress controller, or CDN config to call directly — actually shifting traffic weight is environment-specific (an ALB target group, an nginx upstream, a service mesh's traffic split, etc.). Rather than guessing at one, `canary_deploy.sh` calls an operator-supplied adapter:

```bash
./scripts/canary_deploy.sh --canary-url https://canary.internal:4000 \
  --stable-url https://stable.internal:4000 \
  --lb-adapter ./my-lb-adapter.sh
```

Your adapter is invoked as `./my-lb-adapter.sh set-weight <percent>` and must apply that weight to the real load balancer. Without `--lb-adapter`, weights are printed as instructions ("shift N% of traffic to canary now") and the monitoring/decision logic still runs — useful for dry-running thresholds against a real canary instance before you've wired up automatic traffic shifting.

## Manual rollout (workflow_dispatch)

`.github/workflows/canary-deploy.yml` runs `canary_deploy.sh` on manual dispatch with `canary_url`, optional `stable_url`, `steps`, and `window_seconds` inputs, and uploads `deploy/canary-history.jsonl` as a build artifact. It's dispatch-only — which build to canary and which environment's URLs to target are operator decisions this repo can't make automatically without a build/environment registry.

## Metrics this depends on

`server/src/index.ts` wraps every HTTP request with timing/outcome instrumentation, exposed on `/metrics` (Prometheus text, via `server/src/http/metricsRegistry.ts`):

| Metric | Meaning |
|---|---|
| `qc_http_requests_total` | Counter, total requests handled |
| `qc_http_request_errors_total` | Counter, requests that finished with a 5xx status |
| `qc_http_request_duration_ms_sum` / `qc_http_request_duration_ms_count` | Counters used to derive average latency (`sum / count`) over a window — a summary, not a histogram; percentile latency isn't available from this pair, only the mean |

`/health` also now reports `{ status, version }`, where `version` comes from the `SERVICE_VERSION` env var (set by your deploy pipeline to a git SHA or semver tag) — useful for confirming a canary instance is actually running the build you think it is before trusting its metrics.

## Known limitations

- Latency is an **average**, not a percentile — a canary with a long tail of slow requests but a low mean would not trip the latency threshold. Wire a real histogram/summary library if p99 gating matters more than this repo's dependency-free metrics registry supports today.
- Metrics instrumentation counters (`qc_http_requests_total` etc.) are **process-local and reset on restart** — comparing across an instance restart mid-rollout would understate the window's actual volume. Keep `--window-seconds` well short of any expected restart cadence.
- Rollback here means "traffic weight back to 0%" — it does not itself terminate or roll back the canary instance's deployment; combine with your infra's own instance lifecycle if you need that too.
