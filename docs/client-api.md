# Daemon clients (API version 2)

Boxen's daemon owns execution and retained history. The intended official client is a Rust desktop app; the embedded loopback web page is a temporary client. A custom GUI can implement the same workflow without the official GUI running. No client owns the lifetime of a workload, and no GUI-specific state is needed to discover or control accepted work.

Version 2 makes identity-bearing communication mailbox-based and rejects new direct resident starts. `/api/status.communication` reports `resident: "mailbox"` and `anonymous_thread: "request_response"`. Existing anonymous workload operations retain their behavior. This is an early evolving API, not a promised frozen compatibility layer. `GET /api/status` reports `api_version: 2`, the local machine `identity`, and paired `peers` (`key`, `label`). Requests use the local daemon's loopback origin, a normal matching Host header, and `Authorization: Bearer TOKEN`. The token comes from the daemon's printed control URL fragment or its private `BOXEN_HOME/control-token` file. Never send it to the executing peer, put it in query strings, or log it. It survives daemon restart; deleting the file while stopped revokes existing local clients. Browser clients additionally face the existing same-origin restrictions; native clients can use the loopback API directly.

## Requests

`POST /api/operation`, JSON content type:

```json
{
  "peer": "DESTINATION_PUBLIC_KEY",
  "address": "machine.lan:42424",
  "request": {"op": "list"}
}
```

Use the local `identity` and an empty address for local execution. Remote addresses route to an already paired, pinned identity over authenticated Noise. Local HTTP authentication identifies the local machine's client; the destination records that machine as the workload owner. Different clients on one requesting machine see the same workload catalog. Authenticated network members have full operator access to those runs while stored creation ownership remains unchanged. Identity-scoped runtime tools and connector runners retain their narrower contracts. See [network access and mail](network.md).

Operations:

| Request | Successful response |
| --- | --- |
| `{"op":"list"}` | Array of run snapshots in stable acceptance order |
| `{"op":"list","summary_page":{"after":null}}` | Up to 128 compact run summaries and a `next_after` cursor |
| `{"op":"get","id":"RUN_ID"}` | One network-operator-accessible run snapshot |
| `{"op":"start","spec":SPEC}` | Accepted or previously accepted snapshot |
| `{"op":"thread_import","spec":{"id":"BOXEN_ID","backend_thread_id":"CODEX_UUID"}}` | Read-only history import snapshot; no model execution or resident attachment |
| `{"op":"resident_promote","spec":{"thread_id":"BOXEN_ID","name":"Name"}}` | Resident identity/mailbox attached to the existing conversation; no model execution |
| `{"op":"control","id":"RUN_ID","action":{"type":"steer","text":"..."}}` | `{"accepted":true}` |
| `{"op":"control","id":"RUN_ID","action":{"type":"cancel"}}` | `{"accepted":true}` |
| `{"op":"control","id":"RUN_ID","action":{"type":"respond","request_id":"BACKEND_REQUEST_ID","result":{}}}` | `{"accepted":true}` |

`start` is for identity-less threads; direct resident starts are rejected. Resident correspondence and independent activation use the [mailbox API](mailboxes.md). `SPEC` contains `id`, `thread_id`, `backend` (`codex` or `openrouter`), `cwd` (absolute destination-local directory), `model` (empty for the configured default), and `prompt`. Generate a lowercase 64-character hex run ID before sending, and preserve it with the exact spec until acceptance is known. For a new conversation use that same ID as `thread_id`; follow-ups use the existing thread ID with a new run ID. Repeating the same ID, owner, and exact spec is idempotent. A different spec under that ID is rejected. Catalog discovery by ID can reconcile a response lost after acceptance. A response timeout is not a rejection and must not trigger a new run ID.

Snapshots contain `record` (`spec`, `owner`, `created_at`), `status`, `backend_thread` (nullable), and `last_seq`. `created_at` is a millisecond acceptance timestamp advanced monotonically if wall-clock time ties or moves backwards. Backend thread IDs are runtime references. For built-in OpenRouter execution the reference is the canonical Boxen thread ID, not an upstream provider session handle. Terminal statuses are `completed`, `failed`, `cancelled`, and `interrupted`; the last denotes uncertain execution, not proof that all external work stopped. Active statuses include `starting`, `running`, and `waiting`.

For catalog refresh, pass each response's `next_after` back as `summary_page.after` until it is null. Each page contains `summaries` ordered by acceptance timestamp and run ID. Summaries include a bounded title, run and thread identifiers, resident attachment and import indicators, status, and event sequence. Use `get` for the exact run specification and `watch` for retained events. Older daemons may return the legacy snapshot array to the initial request.

Control acceptance means delivery to the runtime control queue, not confirmed execution of the action. Observe retained control outcomes and the terminal status. Steering and backend responses are not idempotent submissions; do not blindly resend after losing their response. An uncertain earlier run remains `interrupted`; a new message can continue the same thread. The new run records `recovery_notice`, and Boxen passes the uncertainty to the runtime without replaying the earlier work or requiring an acknowledgment ceremony.

## Live observation and recovery

`POST /api/watch` uses the same destination envelope, with `request: {"id":"RUN_ID","after":0}`. The response is newline-delimited JSON:

```json
{"events":[{"seq":1,"at":1234,"kind":"status","data":{"status":"starting","detail":"Accepted by executing machine"}}],"snapshot":{},"done":false}
```

The example omits the snapshot fields described above. Each run has its own contiguous sequence starting at one. The executing machine retains events before publishing them. Batches replay retained history, then wait for new events; empty heartbeat batches are normal. `snapshot.last_seq` can be ahead of the current batch during replay. `done` becomes true only when the terminal run's retained events have all been sent. A terminal stream ends; use the catalog to find future follow-up runs in that thread.

Client recovery algorithm:

1. List runs and group by Boxen `thread_id`, preserving catalog order.
2. Replay each selected run in order. Rebuilding a view uses cursor zero; reconnecting an existing view uses its last applied sequence. Persist a cursor only together with the corresponding projection, otherwise reopen from zero.
3. Apply each event once by `(run_id, seq)`, ignoring duplicates and rejecting gaps. Advance the cursor only after applying the event. Show backend-exposed text, thinking summaries, tool activity, and output immediately. Preserve unknown backend payloads for richer clients.
4. On transport loss retain the last observations, mark the machine unreachable, and retry with backoff. Silence alone does not prove completion or failure. Do not resubmit execution to recover observation.
5. On requesting-daemon restart, reconnect using the retained local capability. On executing-daemon restart, replay the retained `interrupted` event for unfinished work. Completed results remain completed. There is no automatic execution retry or failover.
6. Closing the response detaches the viewer; cancel is a separate explicit operation. Periodically refresh the catalog while idle or on reattachment to discover other clients' runs.

Events include `status`, `backend_thread`, `user_message`, `backend` (original app-server notification/request), `control`, `control_error`, `activation_input` (private resident activation data), `resident_tool_result`, `rpc_response`, and `request_answered`. Full backend event payloads are retained; multimodal transport beyond text is not yet implemented. See [workloads](workloads.md) for bounds and runtime behavior.

HTTP 200 means an operation returned a result or a stream was established. HTTP 401 means the local capability is missing/revoked; reopen the control URL. HTTP 422 is a confirmed operation rejection. HTTP 502 indicates an upstream transport failure with potentially unknown acceptance. Malformed requests may return 400. A stream can report `{"error":"..."}` after headers were sent; reconnect using the retained cursor. Do not infer workload failure from an HTTP status.

Native clients may also speak the existing peer protocol directly when they have their own paired machine identity. Its `watch` request is `{"op":"watch","id":"RUN_ID","after":0}` and returns the same batches. The CLI is a working client of that interface. Named residents and continuity operations are described in [residents](residents.md); they use the same routing and ownership contract.

Diagnostic subscriptions and file control are separately documented in [logging](logging.md). Workload events remain retained even when diagnostics are off. This slice offers neither offline history replicas nor automatic reconciliation of unknown tool side effects.

## Built-in inference events

The authenticated operation `{"op":"runtime_info"}` reports available runtime kinds and configuration readiness without returning credentials. OpenRouter threads use the existing start/list/control/watch operations; resident creation adds optional `backend` (default `codex`). See [inference](inference.md) for configuration, lifecycle, limits and verification.

Built-in event kinds include `inference_attempt`, `inference_frame` (step plus native event), `inference_exchange_completed` (finish reason and usage), `inference_message` (accepted provider message), `assistant_delta`, `reasoning_delta`, `assistant_message`, `tool_arguments_delta`, `tool_started`, `tool_output`, `tool_result`, `tool_outcome_unknown`, and `context_projection`. Text deltas have an item `id` and `text`; a completed `assistant_message` replaces that item's text instead of appending a second copy. Reasoning items have distinct IDs. Tool events carry a run-scoped call ID; raw provider IDs are scoped by step. Unknown native frames can be retained without becoming accepted context. Resident assistant content remains private activity regardless of event kind.

## Observation sources and external adapters

`{"op":"events","args":{"action":"list"}}` reports owned source health,
subscription positions/lifetimes, and any pending-store incident. Registration,
publication, lifecycle controls, bounded preview, and the optional agent query/action
tools are documented in [the observation implementation](events-runtime.md).
`runtime_info.observations` declares the protocol, JSON input boundary, delivery
paths, pending-store bound, and native-session tool-registration limitation.

Accepted run records optionally include `observations`, containing exact prepared
batches with source/subscription generations, timestamps, sequence ranges,
health, explicit gaps, and source-authored data. The shared run stream additionally
retains `observation_batch`, `adapter_call_started`, `adapter_call_result`, and any
`observation_incident`. Missing adapter results remain unknown, not commands to
replay. Anonymous responses and resident private narration keep their existing
contracts. Internal collection/batch timers are inspectable scheduler records;
source/subscription controls manage their policy rather than ordinary schedule
editing. [Scheduling](scheduling.md) remains available independently of streams.

## Messaging connector instances

`{"op":"messaging","args":{"action":"list"}}` advertises the bundled Boxen
transport and owned external instances. The common inbox/send/status operations,
operator configuration, native intake, cursor acknowledgments, dispatch claims,
service outcomes and health are described in [messaging](messaging.md). Instances
scope native accounts, credentials and resident bindings; a provider type is not
a singleton bot or a universal routing namespace. This extends API version 2;
legacy mailbox endpoints retain their original behavior.

## Resident definitions

`resident_definition` accepts an `args` object with `action`: `list`, `get`, `history`, `create`, `update`, `instantiate`, `instances`, or `apply`. It follows the same authenticated destination envelope and shared operator access as resident operations. See [resident definitions](resident-definitions.md) for exact inputs, immutable revisions, manual propagation and runtime pinning. Existing resident, mailbox, scheduling and run APIs operate on instances without a class-specific execution path.

[Resident promotion](resident-promotion.md) preserves an existing native Codex conversation and supplies resident operations through a scoped MCP bridge. The new identity uses ordinary resident/mailbox APIs; historical run records remain unchanged.

## Built-in inference health and continuation

`{"op":"inference_health","offset":0}` returns host-wide operator-visible coalesced incident
rows with pagination. `{"op":"inference_resume","failed_run_id":"FAILED_RUN_ID","id":"NEW_RUN_ID"}`
accepts an explicit continuation in the same canonical conversation. The source
must remain its latest interrupted built-in run; stable exact IDs are retry-safe.
Neither operation replays historical effects or delivers private narration as
mail. See [inference incidents](inference-health.md) for classification, agent
context, recovery semantics and limits.
