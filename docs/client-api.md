# Daemon clients (API version 1)

Boxen's daemon owns execution and retained history. The intended official client is a Rust desktop app; the embedded loopback web page is a temporary client. A custom GUI can implement the same workflow without the official GUI running. No client owns the lifetime of a workload, and no GUI-specific state is needed to discover or control accepted work.

This is an early evolving API, not a promised frozen compatibility layer. `GET /api/status` reports `api_version: 1`, the local machine `identity`, and paired `peers` (`key`, `label`). Requests use the local daemon's loopback origin, a normal matching Host header, and `Authorization: Bearer TOKEN`. The token comes from the daemon's printed control URL fragment or its private `BOXEN_HOME/control-token` file. Never send it to the executing peer, put it in query strings, or log it. It survives daemon restart; deleting the file while stopped revokes existing local clients. Browser clients additionally face the existing same-origin restrictions; native clients can use the loopback API directly.

## Requests

`POST /api/operation`, JSON content type:

```json
{
  "peer": "DESTINATION_PUBLIC_KEY",
  "address": "machine.lan:42424",
  "request": {"op": "list"}
}
```

Use the local `identity` and an empty address for local execution. Remote addresses route to an already paired, pinned identity over authenticated Noise. Local HTTP authentication identifies the local machine's client; the destination records that machine as the workload owner. Different clients on one requesting machine see the same workload catalog. A client using another machine identity does not automatically own those runs.

Operations:

| Request | Successful response |
| --- | --- |
| `{"op":"list"}` | Array of run snapshots in stable acceptance order |
| `{"op":"get","id":"RUN_ID"}` | One owner-authorized run snapshot |
| `{"op":"start","spec":SPEC}` | Accepted or previously accepted snapshot |
| `{"op":"control","id":"RUN_ID","action":{"type":"steer","text":"..."}}` | `{"accepted":true}` |
| `{"op":"control","id":"RUN_ID","action":{"type":"cancel"}}` | `{"accepted":true}` |
| `{"op":"control","id":"RUN_ID","action":{"type":"respond","request_id":"BACKEND_REQUEST_ID","result":{}}}` | `{"accepted":true}` |

`SPEC` contains `id`, `thread_id`, `backend` (currently `codex`), `cwd` (absolute destination-local directory), `model` (empty for the configured default), and `prompt`. Generate a lowercase 64-character hex run ID before sending, and preserve it with the exact spec until acceptance is known. For a new conversation use that same ID as `thread_id`; follow-ups use the existing thread ID with a new run ID. Repeating the same ID, owner, and exact spec is idempotent. A different spec under that ID is rejected. Catalog discovery by ID can reconcile a response lost after acceptance. A response timeout is not a rejection and must not trigger a new run ID.

Snapshots contain `record` (`spec`, `owner`, `created_at`), `status`, `backend_thread` (nullable), and `last_seq`. `created_at` is a millisecond acceptance timestamp advanced monotonically if wall-clock time ties or moves backwards. Backend thread IDs are runtime references, not Boxen thread IDs. Terminal statuses are `completed`, `failed`, `cancelled`, and `interrupted`; the last denotes uncertain execution, not proof that all external work stopped. Active statuses include `starting`, `running`, and `waiting`.

Control acceptance means delivery to the runtime control queue, not confirmed execution of the action. Observe retained control outcomes and the terminal status. Steering and backend responses are not idempotent submissions; do not blindly resend after losing their response. An uncertain earlier run blocks further execution in that thread; inspect it and explicitly start a new thread in this slice.

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

Events include `status`, `backend_thread`, `user_message`, `backend` (original app-server notification/request), `control`, `control_error`, `rpc_response`, and `request_answered`. Full backend event payloads are retained; multimodal transport beyond text is not yet implemented. See [workloads](workloads.md) for bounds and runtime behavior.

HTTP 200 means an operation returned a result or a stream was established. HTTP 401 means the local capability is missing/revoked; reopen the control URL. HTTP 422 is a confirmed operation rejection. HTTP 502 indicates an upstream transport failure with potentially unknown acceptance. Malformed requests may return 400. A stream can report `{"error":"..."}` after headers were sent; reconnect using the retained cursor. Do not infer workload failure from an HTTP status.

Native clients may also speak the existing peer protocol directly when they have their own paired machine identity. Its `watch` request is `{"op":"watch","id":"RUN_ID","after":0}` and returns the same batches. The CLI is a working client of that interface.

Diagnostic subscriptions and file control are separately documented in [logging](logging.md). Workload events remain retained even when diagnostics are off. This slice offers neither offline history replicas nor automatic reconciliation of unknown tool side effects.
