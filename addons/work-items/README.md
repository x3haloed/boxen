# Explicit work-item service

Independent Rust addon for a durable, read-only Azure DevOps mirror and explicit
operator assignment of work to Boxen coder residents. The Boxen daemon and GUI
are unchanged. Boxen owns residents, execution and accepted correspondence; this
service owns work-item observations, immutable assignments, coder bindings and
its own capacity/resource claims. There is no coordinator model in this service.

This is the first manual loop slice. **Polling never creates an assignment.**
Only the local operator `assign` command accepts implementation work. New copies
require `new_copy: true`; updates/returns for existing work retain its coder.
Publication is disabled: all delivery flags must be false. Structured clarification,
independent review gates, publication intent/readback and automatic context-agent/ADO
dispatch remain separate work. Mail bodies are not parsed into operator commands.

## Configuration and installation

This Cargo package builds separately from portable Boxen:

```text
cargo build --release --locked
```

An operator-authored JSON configuration supplies these fields:

```json
{
  "boxen_home": "C:/absolute/private/boxen-home",
  "url": "http://127.0.0.1:42425",
  "owner": "BOXEN_MACHINE_64_HEX_KEY",
  "client_id": "STABLE_WORKFLOW_CLIENT_64_HEX_ID",
  "definition_id": "CODER_DEFINITION_64_HEX_ID",
  "definition_revision": 1,
  "pack_root": "C:/absolute/shared/project-pack",
  "pack_hash": "PINNED_MANIFEST_SHA256",
  "capacity": 1,
  "supervision": {
    "notify_to": {"type":"client","machine":"OBSERVER_MACHINE_KEY","id":"OBSERVER_CLIENT_ID"},
    "start_timeout_seconds": 300,
    "inactivity_seconds": 1800,
    "report_grace_seconds": 60
  }
}
```

Paths are absolute, IDs are lowercase 64-hex, and the HTTP origin is loopback only.
Redirects are disabled. The private Boxen capability is read from its control-token
file, not stored in configuration, requests, evidence or logs. The API machine
identity must equal `owner`. This first version targets that owner's local host;
cross-host worker/resource allocation is not implemented.

Windows installation:

```powershell
./install.ps1 -ConfigFile C:/absolute/config.json -LegacyDatabase C:/absolute/relay.sqlite3
```

The legacy mirror must be disabled and idle first. Initialization copies only the
mirror tables and public ownership/latest-decisions/job context. It never reads
actor credentials. Existing source history stays in the old store; tokens, pending
mail, executing jobs and existing coder native sessions are not transferred.
Initialization preserves imported observations and does not replay old assignments.
The imported Azure configuration supplies project/assignee/marker/board fields and
the polling interval (15 minutes in the installed deployment). No company/project
settings are embedded in this source package.

Defaults are `%USERPROFILE%/.boxen-work-items`, with `bin/boxen-work-items.exe`,
`config.json`, `work-items.sqlite3`, `evidence/`, `inspections/`, `locks/`, and
`logs/service.jsonl`. Task `Boxen-Work-Items` starts at interactive-user logon and
restarts failures. It owns only this service, independent of Boxen and the GUI.
`-NoBuild` installs an existing release; `-DataDirectory` selects another home.
Console events contain operational metadata, never Azure response bodies or
credential output. Logs rotate above 10 MiB on wrapper startup. Private evidence
and SQLite contain work-item/assignment content and inherit local-user privacy.

The read-only Azure adapter uses the existing user's Azure CLI authentication.
On Windows it uses the installed Azure CLI Python executable; the optional
`BOXEN_WORK_ITEMS_AZURE_PYTHON` chooses an alternative. On Unix it invokes `az`.
No credential copying or Azure writes are implemented. The one POST operation is
a read-only WIQL query. Polling and dispatch have separate worker loops, so a slow
Azure scan does not own assignment delivery. Worker and scan locks prevent duplicate
local supervisors/scans. Process deadlines/output limits contain stalled CLI calls.

## Operator commands

```text
boxen-work-items --home ABS status
boxen-work-items --home ABS poll
boxen-work-items --home ABS poll --item 123
boxen-work-items --home ABS packet 123
boxen-work-items --home ABS assign --request-file ABS_JSON
boxen-work-items --home ABS messages --after 0
boxen-work-items --home ABS release --request-file ABS_JSON
```

`packet` returns retained original work item, revisions, comments, linked PR state
and discussions, legacy ownership/decisions, revision, observed time and source
digest. Attachments remain URLs/metadata; their bytes have not been inspected.
Historical revisions/author qualifications and current observations are distinct.
`poll --item` refreshes one item. A busy scan is reported rather than overlapped.

An assignment request has this outer shape:

```json
{
  "idempotency_key": "operator-stable-request-key",
  "item": 123,
  "source_digest": "DIGEST_FROM_CURRENT_PACKET",
  "new_copy": true,
  "context": {
    "decision": "new_implementation",
    "evidence": ["Attributed current context/decision references"]
  },
  "legacy_reconciliation": "Reconciliation of existing owner/PRs/holds, when present",
  "assignment": { "...": "Completed installed project-pack assignment template" }
}
```

`assignment` follows the pinned pack's template: task, acceptance/preservation/holds,
work-item ID/revision/digest, requester, revision, repository/worktree/branch/base,
evidence directory, runtime allocation and delivery scope. Its read-only
`scripts/Inspect-Assignment.ps1` helper runs with a deadline and retained report
before a new request is accepted. The helper must report the pinned manifest and
valid inputs. The service never runs an assignment-supplied executable.

Context decisions are explicitly supplied by the operator for this slice; they are
not a claim that a context-agent decision tool or attributed answer authority exists.
Existing clarification holds require supplied authoritative-answer references.
Historical unfinished ownership prohibits a fresh copy. An adopted legacy coder
must first be imported/promoted by the operator: `new_copy: false` plus `coder_id`
then checks its recorded native thread. A normal existing Boxen copy must match the
configured coder definition. References and cleanup assertions are operator records,
not independent authentication/verification of human answers or resource cleanup.

An explicit `context.decision: "investigate"` supports useful work independent of
unanswered questions. It requires a nonempty `investigation_rationale` and retains
`open_questions` when legacy clarification is pending. Its task names
`mode: "investigation"`, `validation: "read_only"` or `"isolated_synthetic"`, and
`production_edits: false`, `external_writes: false`. Publication remains disabled,
existing coder ownership remains binding, and no legacy question is marked answered.
The accepted scope directs the trusted Full Access worker; it is not an OS sandbox.
Finishing such an assignment is an investigation handoff, not proof of the reported
incident's cause, an implemented fix or work-item completion. Later implementation
still requires its applicable answers/evidence and a new explicit assignment.

An exceptional operator-authorized replacement adds
`replacement: {"expected_coder":"OLD_ID","expected_version":1,"reason":"...",
"authority":"attributed human decision","handoff":"retained evidence/history references"}`
alongside `new_copy: true`, with no `coder_id`. The job must be paused with capacity
released; CLI acceptance rechecks old execution/mail quiescence. Exact expected
owner/version checks prevent stale transfers. A new assignment version and a durable
`ownership.transferred` event retain the prior owner, authority and handoff; old
assignments and residents remain intact. The new copy is created through the pinned
definition with stable retry identity. This is an explicit continuity break, not a
claim that hidden native state transferred. Future continuations keep the new owner.

The transaction accepts one work record per source/item, an immutable assignment
version and frozen packet, deterministic coder creation/message IDs and pending
dispatch. Repeating the exact key returns that accepted version; changed reuse
fails. Later unfinished work uses `new_copy: false`, retains the coder and advances
the version after the previous reservation is released. A coder merely planned
before a failed/stale dispatch retains its original creation intent and ID too.

The full assignment/source packet is projected to an immutable SHA-256-addressed
file under `evidence/JOB/VERSION/packet.json` before mail is sent. Mail carries the
frozen assignment, path and checksum, avoiding silent truncation of large ticket
history. This projection is host-local; remote file transport is not implemented.
SQLite owns the version and can regenerate a missing projection; conflicting bytes
are reported. Accepted Boxen mail remains owned by Boxen, not a duplicate mail DB.

## Capacity, resources and completion

Default capacity is **one dispatched assignment**, retained until an explicit operator
release or a supervised handoff passes the checks below. Configuration permits 1–16 and is loaded at worker startup. A transactional
claim prevents overbooking; pending jobs whose resources conflict do not starve
unrelated later jobs. Canonical worktree paths are exclusive. Runtime-required
assignments additionally claim their explicitly allocated frontend/backend ports,
database and browser-context keys. These are claims among this service's jobs, not
OS locks against arbitrary local processes or an automatically provisioned app/DB.
Pack readiness checks and actual runtime readiness remain separate.

An assignment can be `queued`, `reserved`, `delivered`, `needs_refresh`, `paused`
or `finished`. `delivered` means Boxen accepted the exact assignment mail, not model
execution or implementation completion. Copy creation and mail submission have
retained `submitting`/`uncertain`/`accepted` milestones. Uncertain retries reuse
the same requests/IDs; they do not create another copy or reset its memory. One bad
dispatch retains its reservation and visible error; other available slots continue.

Code progress/handoff replies arrive in the service's Boxen client inbox, read by
`messages` with the returned pagination cursor. Private assistant finals are never
converted to messages or job completion. No manual receipt gate controls later mail.

Release request:

```json
{"job_id":"JOB_64_HEX_ID","outcome":"finished","resources_cleaned":true,"note":"Operator's handoff/cleanup evidence"}
```

`paused` is the alternative outcome. Release checks that Boxen reports the coder
idle and that accepted assignment mail is presented, so queued mail cannot start
after freeing the slot. A per-job lock serializes release with network dispatch.
Uncertain effects require explicit readback and cannot be certified finished.
Cleanup is asserted by the operator; this service never kills app processes or
deletes databases/worktrees. Closing one turn does not automatically free its slot.

## Supervised handoffs and notifications

The optional `supervision` configuration enables monitoring for **new assignment
versions**. Its destination and timeouts are frozen with each version; old assignments
and ordinary historical mail are not retroactively interpreted as structured results.
The destination is a configured typed observer mailbox, distinct from the report
inbox. A desktop bridge can deliver those notifications without new GUI controls.

Assignments describe explicit `boxen.work-report.v1` mail to their authenticated
`reply_to`, linked through `in_reply_to` to the assignment or its report-only follow-up:

```json
{"protocol":"boxen.work-report.v1","job_id":"JOB_ID","assignment_version":1,
 "outcome":"completed","summary":"Result and scope","evidence":["local artifact"],
 "remaining":[],"cleanup":{"no_owned_runtime_resources":true}}
```

Outcomes are `completed`, `blocked`, `needs_clarification`, or `failed`. The service
checks the authenticated resident, owner, destination, correlation and current job
version. First valid reports remain immutable; conflicts/rejected attributed reports
are visible. Retries are deduplicated. Progress stays ordinary mail. Accepted report
receipts and inbox checkpoints commit atomically; private final text is never parsed.

Monitoring uses assignment mail's `presented_in` run and retained run metadata.
Default deadlines are five minutes without observed startup, thirty minutes without
new run sequence activity, and sixty seconds after terminal execution without a
report. Inactivity is an observation warning, not proof of failure. Completed turns
get at most one durable report-only follow-up, explicitly excluding implementation
or test/effect replay. Crashed/interrupted/cancelled runs and unanswered follow-ups
remain unresolved and visible; no new coder or implementation retry is automatic.
Late valid reports can resolve the missing-report condition. Operator pauses,
completion and newer versions end old monitoring without erasing its record.

Each condition produces a durable outbox message with a stable ID. Lost acceptance
responses retry exactly that request; backend acceptance is distinct from human
attention. Repeated failures update bounded health/counters rather than notifying on
each tick. Observation failure, report receipt, unresolved outcomes and release are
available through `status`; `supervise` runs one bounded reconciliation pass.
`supervise --test-notification STABLE_KEY` queues an explicitly labeled diagnostic
for the configured observer; repeating the key reuses the same message, without
fabricating a work-item incident or starting a coder.
The service keeps its existing loop and bounds inbox pages, watch batches and send
batches; slow HTTP operations have deadlines. Storage has no total retention quota.

Automatic release requires a valid report, an idle coder, presented assignment mail,
`runtime.required: false` and the worker's explicit attestation of no owned runtime
resources. This records worker attestation plus authoritative run/mail readback,
not an invented operator confirmation or OS-wide resource inspection. Runtime-required
or unaccounted resources stay reserved with a cleanup notification. A completed report
with fresh source and successful terminal execution can finish the assignment;
a completed report followed by failed execution is explicitly flagged and paused.
Blocked/failed/clarification or stale
source pauses it after cleanup. Neither outcome closes or edits the Azure work item.
No notification acknowledgment gates unrelated eligible work. Source checkouts and
evidence remain available; this service does not delete them.

Revision/comment/PR-discussion changes invalidate affected assignments even when the
ticket revision is unchanged. Invalidation retains coder ownership and any reserved
resources; it never spawns/reassigns automatically. A stale job can be paused, not
certified finished. Already running local work is not remotely cancelled or fenced
by this slice. Publication is disabled, and refreshed assignment/context is explicit.
Freshness is bounded local observation (30 minutes), not an atomic Azure transaction.
Evidence changing after a read remains possible and belongs in later workflow gates.

## Durability and limits

SQLite WAL/FULL transactions retain per-item mirror checkpoints, history, request
deduplication, assignment versions, effects and reservations. Partial source pages
do not advance that item's checkpoint; other items can still succeed. Previously
watched items and PRs remain observed after reassignment, closure or link removal.
Claims survive service restart. Poll failures back off up to 15 minutes; dispatch
failures back off and retain uncertainty. There is no automatic reassignment,
publication, work-item completion or request-body execution.

Bounds include 10,000 watched/candidate items, 16 MiB Azure responses, 60-second CLI
reads, bounded source pagination and a 30-minute scan. Assignment input is at most
128 KiB, operator JSON files 512 KiB, and mailbox bodies 32 KiB. Oversize/conflicting
input fails visibly without accepting a partial assignment. Mirror/history/assignment
retention has no automatic pruning or total disk quota yet. `status` lists up to
200 latest jobs/items and 50 mirror events; the local database retains the full
operator history. Resource names need canonical operator allocation identifiers;
different names for the same external browser/database are not independent resources.

## Verification

```text
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python tests/smoke.py --boxen ABS_BOXEN --service target/release/boxen-work-items.exe
```

Unit checks cover complete pagination/checkpoints, reassignment/closure tracking,
comment/PR-only changes, exact requests, capacity/conflicts, lost-response recovery,
existing ownership, source invalidation, retained packets and continuation.
The smoke suite uses real Boxen HTTP/execution and a controlled Codex backend for
copy/send/reply, private finals, capacity release, restart, same-copy continuation
and explicitly authorized owner replacement without replay or history deletion.
It also covers supervised structured handoffs, automatic idle/no-runtime release,
one report-only recovery from a silent turn, observer notifications and exact-ID
delivery diagnostics. Unit tests cover crash/inactivity, invalid/stale/conflicting
reports, atomic intake/checkpoints, late results, restart and outage/recovery health.
These are distinct from real model/application evidence. The installed deployment
also performs a live read-only Azure poll; no production coding is dispatched by
installation. Windows native is verified; Unix paths are implemented but not yet
verified. Core Boxen/APE are unchanged by this independent addon.
The authorized Linux SSH probe from this Windows host stopped at host-key
verification; no trust check was bypassed and no Linux verification is claimed.

Azure reference: [comments pagination and deleted comments](https://learn.microsoft.com/en-us/rest/api/azure/devops/wit/comments/get-comments?view=azure-devops-rest-7.1).
