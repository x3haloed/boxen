# Scheduling

The executing daemon owns host-local one-time and recurring jobs. Clients can
close, disconnect, or restart without cancelling them. A resident heartbeat is
an ordinary recurring activation with saved directions; it has no separate timer.
Anonymous threads can also receive scheduled requests in their existing canonical
conversation, using the latest accepted backend, model, working directory, and
context settings for that thread. Managed runtimes resume their native session
when available, subject to the existing explicit reconstruction limits.

## Handlers and communication

Two user workload handlers are implemented:

- `{"type":"resident","id":"…"}` activates an existing resident, including
  with an empty inbox. Pending mail is presented as the usual activation batch;
  later arrivals queue for another activation. Directions may be empty. Narration
  stays private, and outgoing communication still requires an explicit send.
- `{"type":"thread","id":"…"}` submits nonblank saved directions as a normal
  request to an existing identity-less thread. Its response, tools, thinking and
  status use the same retained live stream as a manually submitted request.

Jobs belong to the authenticated peer that created them, on the executing host.
The target must already belong to that peer. Each accepted run records its
schedule ID, definition revision, original due time, actual dispatch time, and
number of coalesced ticks. Boxen identifies the current trigger independently of
the saved directions. Codex receives this as per-turn metadata without replacing
its native developer-instruction configuration.

## Time and overdue work

`at_ms` is an absolute Unix time in milliseconds. An absent/null `interval_ms`
means one-time; recurrence is a fixed elapsed interval anchored to `at_ms`.
Intervals range from 1 second through 365 days. These are not cron expressions,
time-zone rules, or promises of exact execution at a deadline. The provisional
form displays local time and converts it to an absolute timestamp.

Overdue one-time jobs dispatch once when this host becomes available. Missed
recurring ticks coalesce into one activation; the next deadline remains on the
original cadence. A busy target or eight occupied host execution slots defers
work rather than starting another run in the same thread. Completion notifies
the dispatcher; a bounded retry also handles other pre-acceptance failures.
Wall-clock changes affect due times. No catch-up storm or automatic retry of an
accepted workload is performed. A short interval can remain continuously busy;
coalescing bounds pending ticks rather than imposing a usage budget.

## Authenticated API and CLI

The existing authenticated peer operation is:

```json
{"op":"schedule","args":{"action":"create","spec":{
  "id":"64-character-lowercase-hex-id",
  "name":"Hourly check-in",
  "target":{"type":"resident","id":"existing-resident-id"},
  "directions":"Human- or resident-authored directions retained verbatim.",
  "at_ms":1793500000000,
  "interval_ms":3600000
}}}
```

The shell client uses the same action object (inside `args` above):

```sh
boxen schedule HOST:PORT PUBLIC_KEY '{"action":"list"}'
boxen schedule HOST:PORT PUBLIC_KEY '{"action":"get","id":"SCHEDULE_ID"}'
boxen schedule HOST:PORT PUBLIC_KEY '{"action":"history","id":"SCHEDULE_ID","after":0}'
boxen schedule HOST:PORT PUBLIC_KEY '{"action":"update","id":"SCHEDULE_ID","patch":{"expected_revision":1,"enabled":false}}'
```

`list` returns metadata without direction bodies, up to `limit` (default 20,
maximum 50). `next_after_id` supplies the next `after_id` cursor; a null cursor
means the final page. `get` returns the full definition and timing state.
`history` returns up to 20 journal entries and approximately 256 KiB after a
sequence cursor, with `next_after`. Full historical definitions are retained.

Creation with the same ID, owner and original definition returns the current
job, even after it was edited or completed. A different definition cannot reuse
that ID. Updates require `expected_revision` and can change name, directions,
`at_ms`, `interval_ms`, and `enabled`. On update only, `interval_ms:0` removes
recurrence. Target is immutable. Pausing leaves the pending deadline intact;
resuming applies the same overdue policy. A completed one-time job needs a new
`at_ms` before it is enabled again. `delete:true` leaves a permanent tombstone.
Pausing, editing or deleting never cancels an already accepted run; normal run
cancellation remains a separate operation. An unaccepted claim is reconciled or
cancelled automatically during an edit, without an acknowledgment ceremony.

Names contain 1–100 bytes, directions at most 32768 bytes, and IDs are 64 lowercase
hex characters. A host supports 256 pending definitions, including paused ones;
completed and deleted histories do not consume pending slots. History retention
and disk quotas for these journals remain future work.

Residents additionally receive `boxen_schedule` in both managed and built-in
runtimes. It uses these actions, with creation automatically targeting the
attached resident. It cannot inspect or change another resident's jobs or target
an anonymous thread. Authenticated human/custom clients can manage both handler
kinds. These controls do not introduce a sandbox or new approval flow.

The provisional GUI exposes a simple Schedules section: selecting a thread or
resident gives creation a target; refresh, edit, pause/resume and deletion use
the same daemon API. Pending submission retries retain their ID and input.
Execution and discovery do not depend on browser storage.

## Durability, recovery and diagnostics

Private append-only JSONL journals under `schedules/` own definitions and timing.
A claim is committed before dispatch. Its deterministic run ID derives from job,
revision and due time. The existing accepted `record.json` and run journal own
execution: a recovered claim links to that accepted run without resubmitting it.
An interrupted accepted run stays uncertain; later work remains available.
A claim with no accepted run can dispatch using its reserved ID. This is not a
claim of exactly-once external tool effects or lossless managed-session recovery.
`last.outcome:"dispatched"` means acceptance, not successful completion; the
linked run's status and events provide the outcome.

A torn final journal fragment is removed before replay. A malformed journal is
isolated with its original evidence preserved. Runtime schedule-write failures
isolate that job, expose `unavailable`, and leave healthy jobs/workloads running.
Original journals remain reserved, including after an unconfirmed create. After
the storage problem is repaired, restarting the daemon reloads timing state and
reconciles any accepted work. Clients can inspect `get`, `history`, list `issues`
and the referenced run rather than assuming a failed reply means nothing happened.

Diagnostic logging remains off by default. Hot-configured `schedule.*` events
cover operations, dispatch, waiting, errors and recovery, correlated with peer,
thread and run where available. Direction bodies require explicit payload
capture. Diagnostic logging does not own timing or execution retention.

## Verification

`scripts/scheduler-smoke.py` exercises two real paired daemons with controlled
Codex responses: live streaming/native resume requests, one-time and recurring
jobs, coalescing, revision conflicts, busy deferral, resident no-send/empty
activations and self-tool scope, offline recovery, crash boundaries, uncertain
accepted runs, storage failure isolation, and idle CPU. Run it against native and
actual APE binaries; the Linux rotation includes the same suite.

`scripts/scheduler-live.py BINARY openrouter|codex [MODEL] [CODEX_PATH]` separately
uses existing local credentials/sign-in for four disposable real-model runs:
seed effect, scheduled append plus recall, a manual follow-up checking trigger
metadata, and an empty-inbox resident activation. It can consume usage and does
not contact existing residents or install production jobs. Controlled fixture success alone
is not evidence of provider compatibility or real model behavior.

Cross-host job transfer, execution-owner relocation, arbitrary pluggable handlers,
calendar recurrence, usage budgets and scheduler retention limits remain planned.

### Verified implementation — November 1, 2026

Rust's 23 unit tests, formatting, Clippy with warnings denied, and the controlled
native macOS suite set passed. Native and APE blocking/notification probes and
actual scheduler daemons passed their idle-CPU checks. Browser automation created,
paused, edited, resumed, dispatched and deleted a schedule without script errors.

The same portable artifact passed all twelve controlled suites on Debian 13
x86-64, plus direct Mac-to-Linux scheduled thread execution and retained replay
over the LAN. Temporary test processes and files were cleaned up. The artifact's
SHA-256 was `77f89107fa4cff88c3622402924f3d23c805f047e4f5318c777a6830301dc374`.
That APE also passed four disposable real-model runs each through OpenRouter
(`openai/gpt-4.1-mini`) and Codex app-server (`gpt-6-luna`), including native
conversation continuity, one scheduled shell effect, the subsequent ordinary
trigger, and resident activation without mail. The Linux scheduling evidence
uses the controlled Codex fixture; it does not establish real-model scheduling on
Linux. Windows and other untested platforms remain unverified. No migration of existing residents
or production schedule was created by these checks.

Internal source-collection and batch-readiness handlers now reuse the same timing
authority for [observation subscriptions](events-runtime.md). Their read-only timing
journals checkpoint every 32 records with monotonic sequence IDs and an explicit
coalesced-history marker. User workload journals and accepted execution records
retain their existing authority and history; this checkpoint is not effect replay.
