# Assigned-browser approval policy

The optional private `approval-policy.json` beside the work-item service config
enables one operator-defined rule. The service evaluates retained Codex
approval requests and answers matching requests through Boxen's existing authenticated
`control` API. The GUI and portable daemon do not gain another request authority.
Codex uses `on-request` with `danger-full-access`; its reviewer remains the client.

```json
{
  "enabled": true,
  "rule_id": "assigned-browser-launch",
  "revision": 2,
  "authority": "Operator-approved assigned browser launch and native Edge access",
  "native_edge_app_access": true,
  "browser_executable": "C:/Example/Browser/msedge.exe",
  "shells": ["C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"]
}
```

Configuration is operator-owned, hot-read and bounded to 16 KiB. Missing or disabled
configuration leaves the normal manual response path. Invalid configuration or
unavailable evidence produces a retained policy error and an observer notice;
it cannot turn a request into an approval. Real paths and deployment identities
belong in private installed configuration.

A match requires the current delivered assignment and coder/version, held capacity,
all current resource claims including the browser reservation, and an owner-matching
Boxen run waiting for input. The request's native thread, active turn and command
item must match the retained run events. Additional permission/network grants are
outside the rule. The requested working directory must resolve to the project pack
or inside the assigned checkout.

The only accepted command is the canonical visible browser launch generated from
the configured executable, assigned persistent profile, frontend port and synthetic
tenant. The assignment exposes it as `runtime.browser_launch_command`. The browser
profile must already exist. A bounded shell-argument decoder verifies the configured
shell, supported startup flags, and the complete exact script. It does not infer
authorization from command substrings, model prose or `commandActions`. Compound
commands, changed quoting/scripts, other executables/profiles/origins, extra arguments
and unrelated approvals do not match. A nonmatch stays available for manual review
and produces an observer notice; no automatic decline or blanket approval follows.

The optional `native_edge_app_access` flag defaults to false. When enabled for an
Edge browser assignment, it also accepts Codex's native Computer Use app-access
request for `msedge.exe`. This permission is app-wide: it can include Edge windows
outside the assigned profile. The same current assignment and reservation checks
apply. The request must be an empty form from `cua_repl`, correlated by call ID
with a retained `js` call from the bundled Computer Use plugin, with matching
native thread/turn metadata and exactly the Edge app parameter. Other apps,
connectors, forms requesting input and unrelated permissions remain manual.
The response is `{"action":"accept","content":{}}`, without persistence metadata.

The service reads the local daemon's authoritative JSONL journal incrementally,
bounded to 512 records/8 MiB per pass and 4 MiB per record. Partial final records
wait for completion; truncation and unavailable evidence require manual review.
This first policy targets a service and daemon sharing one local Boxen home.
Cross-host policy evaluation is not implemented.

SQLite retains the exact request, rule snapshot, assignment binding and durable
intent before submitting `{"decision":"accept"}`. It never sends session-wide
approval or accepts an execution-policy amendment. A per-job operation lock keeps
normal release/continuation from changing the lease during the decision boundary.
The daemon's original pending-request identity remains the final one-time response
authority. Answered/resolved requests disappear from the projection. An unresolved
native app request can outlive a helper timeout; its retained original call
correlation remains available until answered, resolved or the turn ends. Projection
upgrades replay retained evidence while preserving prior decision identities.

`boxen-work-items --home ABS approvals` lists retained decisions. `submitted` means
the control response was accepted into Boxen; the daemon's retained answer and
execution events establish what happened afterward. An interrupted `intent` or
`uncertain` delivery is not automatically resubmitted. Exact retries and restarts
cannot broaden a one-time response into session permission. Manual reconciliation
remains available through the original request. Worktree/database cleanup,
application authentication, browser observations, reviews and PR readiness remain
separate workflow responsibilities.

Tests cover changed commands/scopes, lost reservations, manual decisions, partial
records, uncertain responses and restart. The existing HTTP smoke includes an
actual Boxen request/control round trip with a controlled Codex backend, without
starting a browser. Real model/browser validation is separate evidence.
