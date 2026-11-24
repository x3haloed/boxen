# Agent-facing machine work

`boxen_machines`, `boxen_work`, and `boxen_thread_files` expose the existing
machine network, anonymous conversations, retained run events, controls and
attachment transport. Non-resident workloads additionally receive read-only
`boxen_threads` for discovering and reading earlier non-resident threads across
admitted machines; see [cross-thread history](thread-history.md). They are available to built-in inference workloads and
through a run-scoped MCP connection to managed Codex, Claude and Antigravity workloads. These tools do
not create residents, send mailbox correspondence, distribute credentials or
move a native conversation to another host.

## Discover and request

`boxen_machines` returns the current machine and admitted network members, with
observed reachability and runtime/execution declarations. Names, full machine
IDs and unambiguous ID prefixes are selectors. Eight members are probed per
page; one failed member does not suppress the others. Runtime declarations do
not prove installed application permissions, toolchains or usable inference
credentials. A workload can inspect its actual destination environment.

`boxen_work` has these actions:

| Action | Result |
| --- | --- |
| `list` | A bounded page of anonymous requests, optionally filtered by `thread_id`; `next_cursor` continues the catalog. |
| `request` | An accepted request handle, an explicit rejection, a pre-dispatch failure, or an unknown acceptance outcome. |
| `handoff` | The same request arguments and outcomes, with durable source provenance retained by the destination. |
| `read` | Bounded retained events, a replay cursor, current status and an available conventional response. |
| `steer` | Acceptance of additional input for active work. |
| `cancel` | Acceptance of an interruption request. |
| `respond` | Acceptance of an answer to a retained backend request. |

A new request needs a prompt. An omitted machine defaults to the executing
machine. Remote requests without a working directory get the destination's
automatic workspace; model omission uses the destination runtime's default.
An explicit working directory belongs to the destination filesystem. Backend
omission follows the coordinating workload for a new conversation. Existing
conversation settings supply follow-up defaults.

A request can omit `request_id`, supply a short label, or supply a full request
ID. Omitted IDs derive from the coordinating run and tool call. Labels derive
from the canonical coordinating conversation and survive subsequent turns and
source restart. Returned full IDs remain usable independently of the label.
An exact retry reuses the retained request envelope and immutable attachments;
changed input under that ID is rejected. A fresh label represents new work.

`thread_id` continues the destination's latest accepted request in that
conversation. `after_request_id` instead names an exact predecessor and avoids
a catalog lookup; a stale predecessor is rejected by the execution authority.
Retained request receipts associate handles with destinations, so an omitted
machine on later reads, controls, follow-ups and file retrieval follows a known
handle. An explicit machine selector remains authoritative. Unrecognized
handles default to local lookup rather than searching every network member.

For example, these tool arguments discover a destination, request work, and read
its progress:

```json
{"machine":"Build machine"}
```

```json
{"action":"request","machine":"Build machine","request_id":"build-check","prompt":"Inspect the checkout and report the build result.","cwd":"/workspace/example"}
```

```json
{"action":"read","request_id":"build-check","after":0,"wait_ms":20000}
```

## Observe, continue and recover

Reads use the existing retained event sequence. `next_after` is the last included
event, and `has_more` reports an unread retained tail. `wait_ms` waits for new
activity for up to 20 seconds; detaching the reader leaves execution running.
`done` reports terminal execution, even when events remain to be paged. A
network failure does not establish workload failure.

Event projections have a 48 KiB batch budget and explicitly label truncated
content. `event_seq` retrieves an original event as JSON text in UTF-8 byte
pages, using `offset` and `next_offset`. Conventional responses have 16 KiB
pages, using `response_offset` and `next_response_offset`. Returned offsets are
valid UTF-8 boundaries; out-of-range or mid-character offsets reject. Original
journals remain authoritative independently of these projections and diagnostics.

Request acceptance does not imply completion. A lost start acknowledgment
returns an unknown acceptance outcome; retrying the same request ID reconciles
with the destination's accepted run rather than starting another effect.
Control attempts and their confirmed responses are retained in the coordinating
run. An exact repeated control with a retained result returns it; an attempt
without a retained result remains unknown and is not automatically repeated.
A newly accepted continuation does not replay earlier prompts or tools.

For `handoff`, the destination authenticates the machine that declares the
origin. Its accepted record retains that machine and the reported source run,
conversation, and tool call. The source run retains the accepted destination
handle. The child remains independently observable and continues when the
coordinating run ends. An anonymous request can require the exact acceptance
receipt as its completion condition. [Handoff completion](client-api.md#handoff-completion)

## Files and runtime boundaries

Request `attachments` are local file paths. Original bytes are retained before
transfer and before the outgoing request is dispatched. Cancellation is checked
between transfer chunks and before child dispatch; an already accepted child
keeps its independent execution lifetime. Existing attachment
limits apply: eight files per request, 32 MiB per file, bounded chunk transfer,
and digest verification.

`boxen_thread_files` lists recorded workspaces and retrieves files by `run_id`.
`pull` reads a relative path within a terminal anonymous run's workspace at pull
time. `attachment` retrieves a retained original by digest. Remote retrieval
verifies the bytes in the executing machine's existing attachment store and
returns a locally readable path, with source provenance. Missing sources and
integrity failures remain errors. The original current-conversation workspace
and historical-placement behavior remains available.

Workspace listings echo the selected conversation and responding machine. They
distinguish out-of-scope resident conversations, unknown threads, and eligible
selections with no recorded workspaces. Counts refer to run workspace records,
including repeated paths. `next_offset` and `through_run_id` together continue
the same newest-first selection. Workspace references describe retained records
of working directories. Retrieval checks availability independently.

Work catalog pages report the selected machine and optional thread filter,
how many summaries were scanned, how many matched rows were returned, and
whether `next_cursor` can continue. A filtered page can be empty while the
underlying catalog still has more rows.


Native Codex dynamic tools stay fixed at session creation. The work MCP server
is attached at each run's supported start/resume boundary, so an existing native
conversation can acquire the current interface without reconstruction or changes
to global Codex configuration. The connection is loopback-only, authenticated,
and limited to the run lifetime. Anonymous work bridges expose work tools;
promoted-resident bridges additionally retain their existing identity tools.

Existing paired daemons need only the established authenticated operations
(`runtime_info`, `list`, `get`, `start`, `watch`, `control`, `thread_result`,
workspace and attachment operations). These agent tools add no remote task
service or second placement registry. Existing resident child-request tools
remain compatible. Fine-grained movement authority and platform-specific
execution support remain separate capabilities.

## Verification

`scripts/agent-work-smoke.py` exercises two real paired daemons with controlled
native runtimes, including MCP retries, source restart, native resumption,
immutable attachment retry, destination inference, continuation, controls,
large Unicode response pagination, raw event reads, both native file-tool entry
points, slow-link transfer cancellation and evidence retrieval.
The built-in path is covered by `scripts/inference-runtime-smoke.py`. Both are
included in the Linux rotation and run against native and actual APE artifacts.

`scripts/agent-work-live.py` is an opt-in real-model check using host-local Codex
sign-in. It verifies discovery, child work, an opaque attachment's destination
effect, verified retrieval, continuation and tools after native resumption.
Its default topology is two isolated daemons on one host; that does not prove
Windows, a physical second host, or application-level input/capture permissions.
