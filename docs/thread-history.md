# Cross-thread history (`boxen_threads`)

`boxen_threads` lets a non-resident Boxen workload discover and read earlier
non-resident threads without naming an explicit thread ID up front, without
mutating source threads, and without replaying historical side effects. It is
available across all four runtimes (`codex`, `claude`, `antigravity`, and
`openrouter`) through the shared per-run MCP bridge and built-in runtime tool
interface, backed by the authenticated `thread_history` operator operation.

Model-facing cross-thread continuity is experimental and off by default. The
desktop **New Thread** command starts an ordinary thread. Setting
`BOXEN_EXPERIMENTAL_THREAD_CONTINUITY=1` in the environment of `boxen serve`
enables the tool and its context for eligible threads executed by that daemon,
including threads started from the desktop. The authenticated `thread_history`
operator operation is available with either setting.

## Scope and resident exclusion

Cross-thread discovery and reading are strictly scoped to **non-resident**
threads:

- Any conversation registered as a resident, or containing any run with a
  resident attachment or `resident_id` (including promoted threads whose early
  runs predated promotion), is excluded from `list`, `search`, `read`, and
  session-start orientation.
- Resident activations do not expose `boxen_threads` in `tools/list`, do not
  receive cross-thread orientation or `boxen_threads` runtime framing, and are
  rejected if they attempt to invoke it.
- The calling non-resident thread is excluded from `list` and `search` on
  each queried machine so retained copies of the same thread do not echo its
  prompt back at the model.

## Actions

| Action | Behavior |
| --- | --- |
| `list` | Returns other non-resident threads newest-first with UTC timestamps, human ages (`2h ago`, `8d ago`), run counts, latest status, and opening/latest previews. Supports `since_hours`, `until_hours`, `cwd`, `limit`, `offset`, `max_bytes`, and optional `machine`. |
| `search` | Queries this machine and admitted peers by default. An explicit `machine` limits the search to one host. Each network hit includes its source `machine` and `thread_id`, which together address a transcript. |
| `read` | Projects a clean turn-oriented transcript across `codex`, `claude`, `antigravity`, `openrouter`, and `thread_import` run journals. Preserves terminal/uncertain statuses (`interrupted`, `failed`) and `status_detail`. Compact tool summaries fit long threads into a single read; `include_tool_output: true` expands tool bodies. |

When `read` is called without `start_turn` on a thread that exceeds the byte
budget (default 24 KiB), Boxen retains turn 0 (opening intent) and packs as
many latest turns as fit, reporting the exact omitted middle range in
`omitted_middle` so late reversals and final statuses remain visible on the
first read. Callers can page any range with `start_turn`, `limit`, and `order`.

Each queried machine searches its own retained journals using IDF-weighted
multi-term scoring and returns bounded candidates. Network search merges them
by exact phrase match, number of query terms matched, local rank, then recency.
The requested `limit` and `max_bytes` apply to the merged response. Up to eight
hosts are queried concurrently per batch. Failed or unsupported peer requests
leave available results usable. `complete_host_coverage` is false in that case,
and the response includes counts and up to eight unavailable machine IDs.
An empty partial search leaves unqueried history unknown.

`list` and `read` default to the current machine and accept a source
`machine` explicitly. A moved thread may have retained history on more than
one host. Search hits identify the host that supplied the matching history.

## One-time session-start orientation

With the experiment enabled, Boxen appends a bounded one-time summary on the
opening run of a new non-resident thread (`after` absent and `id == thread_id`).
The summary includes up to eight
recent non-resident threads (ID, age, run count, latest status, opening
preview) in Boxen-authored runtime context. Follow-up runs in the same thread
omit that list to preserve prompt-prefix stability while retaining full
`boxen_threads` search/read access for mid-conversation pivots to any earlier
thread.

## Authenticated API and diagnostics

Clients and paired peers can invoke the host-local projection directly. The
receiving daemon queries its own journals, so authenticated peer requests do
not trigger another network search.

```json
{"op":"thread_history","args":{"action":"search","query":"webhook rotation"}}
```

`thread_history.operation` diagnostic records follow Boxen's standard
off-by-default logging policy (`Target::Workload`), emitting action,
thread ID, outcome, and returned counts without logging private payloads when
payload capture is disabled. Run journals remain the sole authority; no second
mutable thread index is maintained.
Network search also records aggregate coverage and individual failed hosts
through `thread_history.network_search` and
`thread_history.search_host_unavailable`.

## Verification

- Rust unit tests in `src/thread_history.rs` verify direct and promoted
  resident exclusion, near-miss search ranking, and interrupted-status detail
  preservation.
- `scripts/thread-history-smoke.py` runs two real paired daemons with
  controlled runtimes (no credentials), exercising multi-backend `list`,
  `search`, `read`, time filters, head+tail budgeting and middle paging,
  strict direct/promoted resident exclusion, default network search with
  combined results, transcript reads using the machine ID from the hit,
  partial coverage with an offline peer, local and peer MCP tool access,
  one-time session-start orientation, resident-caller exclusion, ordinary New
  Thread behavior with the default settings, and runtime diagnostic logging.
  It is included in `scripts/test-linux.py`.
- Live multi-model hill-climb verification (disposable seeded corpora plus a
  read-only copy of a real local non-resident thread) evaluated continuation,
  terse openers, long late-reversal threads, multi-thread pivoting/retention,
  negative controls, and resident isolation across Codex (`gpt-6.1-sol`) and
  Antigravity (`gemini-4-neon`, `claude-sonnet-5-5`).
