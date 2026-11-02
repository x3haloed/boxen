# The first resident

A resident can have a name and a home without having a job. Creating one saves its identity and memory; it does not start inference or require a task description. Each resident currently lives on one explicitly selected machine with one canonical Boxen thread, backed by Codex or the built-in OpenRouter runtime. Ordinary scheduling supports heartbeat activations. [Resident definitions](resident-definitions.md) now provide reusable templates with independent instances and manually applied revisions. Movement, replication, shared mutable memory, parallel conversations and reintegration remain beta requirements.

## Mailbox communication

Every resident has its own durable mailbox. Incoming messages, activation, and explicit sending are separate; assistant output is private narration. Automatic activation on new mail is enabled by default. Anonymous threads retain their distinct request/response behavior. [Mailboxes](mailboxes.md) documents the implemented delivery, batching, API, tool, and migration contracts.

## Use it

Connect to a machine in the provisional controls, expand **Create a resident**, and enter a name, destination-local working directory, optional model, identity, memory, and continuity quota. The resident appears in the thread selector even before its first conversation. Select it and send a message. Messages enter its mailbox and are presented to the canonical conversation in activation batches. New mail during execution queues for the next activation. Cancellation, backend input responses, replay, and reconnect remain operational controls; resident steering is rejected as a correspondence bypass.

**Resident identity and memory** shows stored revision and usage. Save uses the editor's revision, so an agent's intervening write produces a visible conflict instead of silently overwriting it. Your edits stay in the form; **Reload stored values** explicitly replaces them with the current state. Names need not be unique; IDs are the authority.

Shell equivalents (MODEL `-` uses the executing machine's configured default):

```sh
boxen resident-create HOST:42424 PEER_KEY Pip /absolute/workspace - --identity-file identity.txt --quota 65536
boxen residents HOST:42424 PEER_KEY
boxen resident-get HOST:42424 PEER_KEY RESIDENT_ID
boxen resident-message HOST:42424 PEER_KEY RESIDENT_ID "Hello" --subject "Greeting" --id MESSAGE_ID
boxen inbox HOST:42424 PEER_KEY
boxen resident-activate HOST:42424 PEER_KEY RESIDENT_ID --id RUN_ID
boxen watch HOST:42424 PEER_KEY RUN_ID
boxen resident-update HOST:42424 PEER_KEY RESIDENT_ID EXPECTED_REVISION --memory-file memory.txt
```

Creation supports `--id`, `--identity-file`, `--memory-file`, and `--quota`. Preserve the original ID and exact inputs when retrying an uncertain creation or message. Resident creation remains idempotent even after the resident updates its memory. Identity/memory edits use compare-and-set; on an uncertain response, read the current revision/content rather than blindly overwriting it. Working directory and model are pinned in this first implementation; name, identity, memory, and quota can be edited. Anonymous threads remain available.

## Continuity and environment

The resident ID is its canonical thread ID. Run IDs and backend thread IDs remain distinct. Only one run can execute in the canonical thread at a time. A request cannot use that thread anonymously, point the resident at a different thread, or change its pinned runtime settings. The authenticated creating machine owns API access, following the same trust model as anonymous workloads.

At acceptance, Boxen captures the identity/memory revision into the immutable run record. The runtime receives that snapshot plus the executing machine ID, actual runtime OS and architecture, destination working directory, and the durable-store location. Cosmopolitan host detection uses its runtime ABI rather than Rust's Linux compile target. Machine-local files and the working environment are explicitly distinguished from managed continuity.

The resident has a daemon-serviced `boxen_continuity` dynamic tool:

- `{"action":"read"}` returns fresh continuity, revision, quota, usage, and host information.
- `{"action":"write","expected_revision":3,"memory":"..."}` replaces memory at that revision. `identity` can also be supplied; omitted fields stay unchanged.

The tool is bound to the active resident and backend thread. It cannot name another resident, raise its quota, rename it, or relocate it. Quota/conflict errors return to the model as tool failures; they do not end its conversation. Tool calls and their results are retained and streamed, not surfaced as approval requests for a human to answer. External writes become visible to an active resident when it reads the tool; the next run gets a fresh accepted snapshot. There is no push invalidation or cross-machine memory synchronization yet.

## Storage and limits

Resident state is stored in `BOXEN_HOME/residents/RESIDENT_ID.json`, independent of backend session files and the working directory. The versioned JSON envelope contains the immutable creation specification for retry reconciliation and the current resident state. Updates write and sync a private temporary file, then replace the snapshot atomically before publication. The daemon's existing authority lock covers resident state as well as run journals. Unpublished temporary files are ignored after a crash.

The enforced continuity quota counts UTF-8 bytes in the current name, identity, and memory combined. It defaults to 64 KiB and is configurable from 1 byte through 256 KiB in this slice. Reads report `used_bytes` and `quota_bytes`; oversized writes leave state and revision unchanged. Agent tool writes cannot raise this limit. Full access is not an OS sandbox: trusted processes can bypass daemon APIs and edit backing files directly, which is unsupported.

The quota is not a total disk limit for the resident. The immutable creation input, accepted context snapshots, conversation journals, backend-owned session files, and diagnostic files are separate retained records with no retention policy yet. Arbitrary portable files and attachments are not implemented. These representations make identity, memory and observed history backend-independent, but copying them does not implement a safe handoff. There is no cross-machine import/activation or mobility API; a resident file pinned to another machine is not automatically activated.

## Stay available after an incident

Interrupted execution remains an uncertain historical fact. A resident accepts its next message without a receipt or acknowledgment gate. The new run retains `recovery_notice` and tells the runtime that previous tools may already have run. It does not automatically resubmit the old prompt or repeat its tool calls.

Boxen normally resumes the backend's existing conversation. If that backend explicitly rejects session resume, Boxen starts a new backend session beneath the same canonical Boxen thread. It records `backend_session_rebuilt`, supplies current continuity and a bounded retained-history excerpt, and identifies the full journal directory for inspection with the runtime's file/terminal tools. The excerpt keeps roughly 48 KiB of recent event JSON, marks oversized individual payloads, and reports omitted older events. Full history remains on disk. This reconstruction does not claim to recover hidden backend state or fit an unlimited conversation into a model context window. Transport timeouts are not treated as explicit resume rejection; they retain their uncertain/failure outcome for the next recovery opportunity.

A malformed, unsupported, or misplaced resident snapshot is preserved and isolated on startup. The daemon retains a metadata incident in `residents/incidents.jsonl`, reports it through `resident_issues`, and continues serving healthy residents and anonymous work. The provisional UI shows those incidents. Restoring a known snapshot and restarting can recover the affected resident; Boxen does not invent a replacement identity or erase the bad file. Existing workload-journal interior corruption still requires repair; this slice does not claim general storage repair or power-loss certification.

## Client API

Use the existing destination envelope at `POST /api/operation` or the equivalent authenticated peer operation:

| Operation | Inputs | Result |
| --- | --- | --- |
| `resident_create` | `spec`: `id`, `name`, `cwd`, optional `model`, `identity`, `memory`, `quota_bytes` | Saved resident; no inference |
| `resident_list` | None | Owner-visible residents, including idle ones |
| `resident_get` | `id` | Current resident |
| `resident_update` | `id`, `patch`: `expected_revision` and optional `name`, `identity`, `memory`, `quota_bytes` | Committed resident or explicit rejection |
| `resident_issues` | None | Current host's resident-load incidents (metadata only) |
| `resident_activate` | Resident `id`, `activation_id`, optional `directions` | Independent activation snapshot; existing run/event streaming contract |

Resident responses include `id`, `thread_id`, `owner`, `machine_id`, `name`, `identity`, `memory`, `cwd`, `model`, `quota_bytes`, `used_bytes`, `revision`, `created_at`, and `environment`. The daemon constructs resident activation runs with the resident’s saved backend, `cwd` and `model`; external direct starts are rejected. Its immutable run record adds `resident` (accepted continuity snapshot) and, when relevant, `recovery` (incident context). Normal run/event APIs transport the complete conversation without depending on the official GUI.

## Verification

`scripts/resident-smoke.py` uses two real daemons and the explicitly test-only Codex fixture. It covers idle creation, idempotency, owner isolation, concurrent compare-and-set edits, quotas, context injection, real dynamic-tool request/reply routing, memory across daemon and runtime restarts, canonical execution guards, uncertain-outcome continuation, missing-backend-session reconstruction, and isolation of malformed resident state. These tests establish daemon behavior, not model understanding. On October 31, 2026, the complete native and identical-file APE smoke suites passed on macOS ARM64, including host-OS detection, runtime timing/idle CPU, daemon logging, anonymous workloads, and recovery regressions. Browser checks exercised idle resident creation, resident-owned memory writes, stale human-edit rejection, explicit reload, and reopening the canonical conversation after requesting-daemon restart.

The same APE was deployed to both physical Macs. A real Codex resident on the Mac mini saved a unique memory through `boxen_continuity`; after restarting its daemon/runtime, it read the exact memory and reported macOS from its current host context. The two runs retained the same canonical Boxen thread and backend conversation, with 141 retained events across the two runs and each run’s sequence preserved. New resident diagnostic events were also captured with content off. Missing-session reconstruction and malformed-state isolation were verified with the deterministic backend, not by deleting real Codex session state. Other operating systems and cross-machine resident mobility remain unverified/unimplemented respectively.

## Promote an existing conversation

[Promotion](resident-promotion.md) attaches a resident to an existing ordinary Codex conversation, including an imported thread, without starting inference. The native thread and saved instructions remain intact; resident tools use a run-scoped MCP bridge. Promotion has an explicit retry-safe CLI/API and adds no GUI controls.
