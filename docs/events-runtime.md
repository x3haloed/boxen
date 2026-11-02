# Observation runtime: first implementation

This is the implemented host-local JSON slice of the [agreed design](events.md).
It provides bounded collection and delivery, optional activation, independent
consumer positions, lifecycle controls, and external query/action tools. The
usual authenticated peer API and local control proxy own access; custom clients
use the same operations as the CLI and agent tools.

## Register, subscribe, inspect

`boxen events HOST:PORT PUBLIC_KEY JSON` sends `{"op":"events","args":JSON}`.
Actions are `list`, `source`, `subscription`, `create_source`, `subscribe`,
`update_source`, `update_subscription`, `publish`, and `read`.

`create_source` has a `spec` with:

- `id` (64 lowercase hex), nonblank `name` (at most 100 bytes), existing absolute
  `cwd`, and optional `command` array (executable plus arguments, at most 32).
- `mode`: `ordered` (default) or `latest`.
- `interval_ms`: collection cadence, 1000–86400000, default 60000.
- `timeout_ms`: process deadline, 100–10000, default 5000.
- `tools`: at most 16 `{name,description,kind,parameters}` declarations. `kind`
  is `query` or `action`; optional `parameters` describes domain arguments.
  Names are at most 64 bytes and descriptions 1000 bytes. Declarations require
  an adapter command. A source definition is limited to 8 KiB.

An empty command creates a publishing-only source. `subscribe` has a `spec` with
`id`, `source`, and `target:{type:"resident"|"thread",id}` for an existing owned
resident or identity-less thread. Optional fields:

| Field | Default / meaning |
| --- | --- |
| `interval_ms` | 60000; batch cadence, 1000–86400000 |
| `waking` | false; ready data may request activation |
| `health_waking` | false; independently permits health notices to request activation |
| `lifetime` | `activity`; also `persistent` or `until` |
| `expires_at_ms` | activity defaults to one hour; until requires a future deadline; persistent has none |
| `wake_limit` | 60 accepted batch presentations per subscription/hour |
| `target_wake_limit` | 120 aggregate presentations/hour; 1–3600 for both limits |

The existing scheduler owns source and batch timers. Their managed definitions
are inspectable through schedule get/history; ordinary schedule create/update
cannot replace their registration policy. Source/subscription controls own pause,
expiry and waking. Cadences are fixed at registration in this slice; a new
definition selects a different cadence. Source collection requires
an active consumer. A first poll can occur before subscription and the next poll
then follows the configured cadence; registration's optional top-level `at_ms`
sets the first timer. The same ID and original input make registration retries
idempotent. A stopped definition is not revived by retrying its creation. Source
and timing writes are separate: a rejected or lost registration reply can leave
a reserved definition. List/get and a retry with the same input reconcile setup;
changed input under the same ID is rejected.

`list` reports owned source metadata, health and subscriptions, plus any storage
unavailability. `source` / `subscription` with `id` return full state. The
provisional GUI has a read-only observation inspector. Numeric clocks are Unix
milliseconds; source observation time and daemon receipt time remain separate.

## Delivery and retention

A timer captures a subscription cutoff independently of inference. Ready ordered
input includes observations through that cutoff. A pending current-state batch
uses the freshest retained snapshot when execution becomes available. Multiple
ready subscriptions can accompany one activation. Batch selection prefers the
least recently presented subscription to avoid starvation. Acceptance consumes
that readiness opportunity; a truncated remainder waits for another timer or
explicit read. Automatic target activations also respect the smallest active
subscription interval since the last presentation. Later data remains subject to
its own cutoff. Empty batches cause no inference; explicit heartbeat jobs remain
independent.

Each source retains at most 64 observations and 64 KiB of serialized observation
state. Each payload is limited to 8 KiB; replies to collection/publication contain
at most 64 observations and 64 KiB. Cursor state is at most 8 KiB. Old data is
evicted under these bounds. Per-consumer batches show explicit missing or
superseded sequence ranges, rather than claiming complete delivery. Source
`dropped` counts evictions, which need not be losses for a consumer that already
received them. A source supports at most 20 accepted publications per second.

One subscription contributes approximately 10 KiB of observation rows per batch;
a run accepts at most 32 KiB of combined batches. Pending state uses an atomic,
synced-file replacement under `observations/pending.json`, capped at 4 MiB. This
first version supports 32 source and 128 subscription definitions including
stopped records. Definition reclamation/configurable capacity and larger-scale
storage remain future work.

Run acceptance retains exact batches in `record.json` before positions advance.
The model input labels these as source-authored observations, distinct from the
request author, instructions, and mail. A resident records activation reason `observations`, keeps private narration and
explicit outgoing sends; a thread keeps normal request/response behavior. Busy
targets wait without overlapping runs. Waking disabled still permits pending
batches on a later ordinary activation.

`read` previews already-ready input without advancing positions. The runtime tool
`boxen_observations` can explicitly prepare a current batch and retain it in an
`observation_batch` journal event before advancing. This provides an explicit
boundary inside an ongoing run. Presentation establishes prepared/retained input,
not model comprehension. Restart reconciles both acceptance records and journaled
tool presentations, without dispatching historical calls. Full accepted history
remains outside the pending-storage quota and follows existing continuity rules.
These guarantees have process-crash/restart evidence; power-loss behavior is not
claimed verified.

Wake budgets conservatively count newly accepted batch presentations, including
manual runs/tool reads, once per advanced position. Reconciliation does not count
the same position twice. The smallest configured active target limit applies;
recent presentations from stopped subscriptions still contribute, preventing
replacement subscriptions from resetting the aggregate budget. Limits apply to
automatic wake eligibility, not unrelated messages or manually requested work.
Subscription windows reset after one hour and are inspectable. Multiple batches
in one activation can contribute multiple presentations. `condition:wake_limit`
reports per-subscription limiting; counters/limits expose aggregate limiting.

## Health and lifetime

Health is separate from observation items. It retains current condition,
first/latest failure, last success, cumulative and consecutive failures, earliest
retry eligibility, and transition counts. Retry backoff begins at two seconds,
doubles on consecutive failure, and caps at five minutes; probes also wait for
the source's ordinary collection cadence. Success resets the consecutive count.
Quiet successful collection is healthy even with zero observations.

Only one frozen health notice is retained. Notices are emitted at most once per
minute, including initial availability and flapping. Suppressed transitions
coalesce; the latest condition can be announced at a subsequent batch tick after
the cooldown. A consumer that already received a failure does not receive each
retry. `health_waking:false` prevents outages from becoming unsolicited wakes.
Current health remains inspectable even when no notice is due; data batches
also identify current source health separately from a frozen historical notice. Structured adapter
error codes, process exit/deadline errors, malformed output and excessive output
remain failures rather than observations. Domain rejections belong in supported
query/action results.

Updates have `id` and `patch.expected_revision`. Source generation serves as its
revision; source patches contain `enabled`. Subscription patches can change
`enabled`, `waking`, `health_waking`, `wake_limit`, `target_wake_limit`, and finite
`expires_at_ms`, or set `end:true`. A pause can resume before expiry. An ended or
expired subscription requires a new ID, and creation retries never renew it.
Expiry is effective immediately in access/delivery checks; the dispatcher records
the ended state on its next wake. Routine turns do not extend the deadline.

Stop/expiry and source-generation changes reject stale callbacks and interrupt
in-flight adapter invocations. A stopped consumer receives no new delivery;
shared sources can continue for other consumers. Source pause/resume increments
generation, so old publishers/calls cannot revive it. Accepted effects can remain
uncertain rather than being reversed or repeated.

Storage failures isolate observation collection and automatic delivery. Healthy
mail and ordinary workloads continue. Original evidence is preserved; after repair,
a daemon restart reloads pending state and reconciles accepted input. An unavailable
store cannot repeatedly activate the same unacknowledged batch.

## External process protocol

One invocation receives one JSON request on stdin and returns one JSON response
on stdout, then exits. Commands are argument arrays, not shell interpolation.
The protocol is `boxen-adapter-v1`:

```json
{"protocol":"boxen-adapter-v1","operation":"collect","source_id":"…","generation":1,"cursor":null}
```

```json
{"protocol":"boxen-adapter-v1","observations":[{"id":"stable-report-or-frame-id","observed_at_ms":1793500000000,"payload":{"value":42}}],"cursor":{"position":1}}
```

Collection is a read-only, repeat-safe adapter contract. IDs deduplicate against
retained observations; adapters use their cursor to avoid rediscovering older
reports after eviction. A bounded adapter cursor cannot establish upstream
history outside the provider's available feed window.

The runtime tool `boxen_adapter` receives `subscription_id`, current
`source_generation`, declared `tool` name and `arguments`. It sends:

```json
{"protocol":"boxen-adapter-v1","operation":"call","source_id":"…","generation":1,"call_id":"RUN_ID:CALL_ID","tool":"move","arguments":{"direction":"east","revision":1},"cursor":null}
```

A supported reply contains `result` and optionally fresh `observations`/`cursor`.
An outer `error:{code:"bounded_ascii_code"}` reports adapter failure; a supported
`result` can report a domain rejection with fresh state. The adapter owns domain
validation and stale-state checks. Boxen advertises declarations rather than
interpreting game rules or validating their optional argument schemas.

At most four invocations run concurrently, serialized per source. Input is at
most 32 KiB; stdout is bounded at 64 KiB. Deadlines and cancellation terminate the
owned process group on tested macOS/Linux paths. Stderr is not copied into model
context. Process boundaries, accepted input, failures and outcomes have structured
diagnostic events; payload capture remains explicit and secrets are redacted.

The existing run journal retains call-start and result receipts. A repeated call
with a missing historical result stays unknown and is not dispatched again.
Timeout/interruption may follow an already completed effect. Killing the daemon
can leave an external process running; adapter/device watchdogs remain responsible
for physical-control safety. Recovery does not replay the old action or require
manual acknowledgment before unrelated work continues.

For independent producers, authenticated `publish` includes source `id`, current
`generation`, a monotonically increasing `sequence` starting at 1, and `response`
in the same observation shape. Retries at an already accepted sequence are
reported as duplicates without replacing data; this is not verification that a
retry's payload matches the original. Sequence gaps are rejected. Source ownership
uses the authenticated machine peer; source-specific delegation/credentials are
not implemented. Native adapter dependencies are optional and host-local.

## Runtime support and verification

Both built-in inference and new Codex native sessions receive observation and
adapter tools, including anonymous threads. Codex registers dynamic tools at
native session creation; its resume API does not add new tool declarations.
Older native sessions can receive batches but do not gain these tools simply by
resuming. No implicit native-history reconstruction is performed just to add tools.

This slice handles JSON observations. Automatic native image/audio/video attachment
projection, persistent adapter processes, arbitrary mid-generation injection,
cross-machine source mobility, richer subscriptions, and a broader desktop UI
remain planned. Existing native media-reading tools remain available.

`scripts/events-smoke.py` uses real paired daemons, external Python processes and
controlled Codex behavior. It covers USGS-format HTTP input, passive/waking and
independent consumers, real game moves and stale rejection, retention loss,
producer duplicates, expiry, unattended failure/backoff, recovery/flapping,
oversized output, stop and crash with uncertain effects, bounded internal timer
storage, hot diagnostics, storage isolation, restart and idle CPU. The Linux
rotation includes this suite and a direct-LAN observation transfer.

`scripts/events-live.py BINARY openrouter|codex [MODEL] [CODEX_PATH]` uses existing
credentials/sign-in for disposable game and live public-USGS tests. It records
filtered game action/result evidence under ignored `work/`. A model ending a game
attempt early is a failed gameplay check, not evidence of a transport failure.
Model success, controlled-provider checks and platform evidence remain distinct.

### Verification evidence — November 1, 2026

The first slice passed 27 Rust unit tests, formatting and Clippy with warnings
denied. Native macOS regression checks and actual APE blocking/notification,
adapter lifecycle, cancellation, crash recovery and idle-CPU checks passed. The
browser inspector displayed remote source state and cleared it on a peer switch.
An already-reconciled presentation also leaves pending storage untouched, avoiding
repeated snapshot writes while replaying a resident's older history.

The same portable artifact passed all thirteen controlled suites on Debian 13
x86-64, then transferred observations from the Mac over the direct LAN into an
existing Linux thread. Retained positions, ordinary responses, streaming/replay
and remote cleanup passed. SHA-256:
`3a7041e7618de307c8e0121a7fa8bf3d45bb74037fb52ad162466a2cc22b9912`.
These Linux checks use controlled backend behavior, not live model inference.

That APE also passed three fresh real-model runs each through OpenRouter
`openai/gpt-4.1-mini` and Codex app-server `gpt-6-luna`: seed a thread, finish the
external game with real persisted moves, and privately describe live USGS input.
Each game used five adapter calls and each observer received five real reports.
These are capability proofs, not estimates of gameplay success rates. Earlier
live attempts included incomplete games; their existence is not erased by later
successful runs.

The original live observer check required an exact event ID. One OpenRouter
narration instead accurately reported the supplied places and magnitudes, failing
that ID-only check. Its raw narration and failure remain under ignored `work/`.
At a verification checkpoint, epoch 2 added exact place plus explicitly labeled
magnitude in the same sentence, while retaining exact-ID acceptance. Authored
anchors reject generic text, wrong/swapped pairs, distance-only numbers and
independent fetching. Fresh conversations for both runtimes then passed epoch 2;
the old run was not retroactively counted as a passing test. This checks reported
input facts, not comprehension or complete event recall.

Windows, other untested architectures, native stream-media attachment projection,
older Codex-session tool upgrades, and long-duration resident behavior remain
unverified or planned as described above. No contact with or migration of existing residents or production
subscription was part of verification.
