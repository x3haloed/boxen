# Mailboxes and workload requests

Identity attachment fixes the communication contract. Every resident has a mailbox by default. Anonymous threads remain conventional request/response workload channels. The daemon enforces the split: new direct resident `start` submissions and resident steering are rejected; accepted historical submissions can still be discovered and retried idempotently without re-execution.

## Receipt, activation, and sending

A message is immutable communication retained in the host mailbox journal. Its envelope includes a lowercase 64-character hex ID, authenticated sender, destination, subject, body, reply-to address, optional in-reply-to ID, accepted timestamp, host machine ID, and sequence cursor. Retry with the original ID and exact contents returns the original message. Conflicting reuse or sender spoofing is rejected. Sender identity comes from the authenticated client machine/client namespace or the executing resident, not from a sender field supplied by the caller.

Addresses are typed:

```json
{"type":"resident","id":"RESIDENT_ID"}
{"type":"client","machine":"AUTHENTICATED_OWNER_MACHINE_KEY","id":"CLIENT_ID"}
```

Mailbox sequence cursors are host-global: gaps in a filtered inbox are normal, unlike the contiguous per-run execution event sequence. Client IDs are stable lowercase 64-character hex identifiers. Multiple clients on one owning machine can use separate return queues. They share that machine's authenticated API authority; these namespaces are not isolation between mutually untrusted users. Resident and client addresses on this host must belong to the authenticated owner's domain. This implementation does not route to other hosts or rooms. Reply-to describes an address, not an access grant.

The receiving host durably retains an accepted message before returning it. `accepted_on` identifies that host. A successful send means queued there, not that a recipient read, understood, acted, or replied. A failed connection before acknowledgment leaves acceptance uncertain; retry keeps the same ID. A request to an unreachable executing host is not saved in a local outbox by this slice.

Every new or migrated resident defaults to `auto_activate: true`. Creation alone does not start inference. New mail notifies an event-driven dispatcher; there is no progress polling loop in the daemon. A resident with no active run receives a batch of queued messages in its next activation. Mail arriving during an active run stays queued; after the run ends it is coalesced into a subsequent activation rather than steering the current turn. Disabling automatic activation retains mail without running it. Manual activation can consume pending mail or run with no incoming messages and optional human-authored directions.

The immutable activation run record contains `mailbox.message_ids`, `reason`, and `directions`. That record, retained before inference, is the authority for presentation. Mailbox reads project `delivery: queued` or `presented`, plus `presented_in` run ID. Presented means included in recorded activation input, not proof of understanding. An explicit reply's `in_reply_to` establishes its relationship to an earlier message; finishing a turn does not mark messages answered or handled.

## Private activity and explicit delivery

Resident assistant text, including final output, is private execution narration. It remains in the owner-authorized execution journal and live activity stream. It is never automatically converted into mail. A resident can finish an activation without sending anything, or reply selectively to a batch. The provisional GUI separates **Messages** from collapsed **Private execution activity**. Operational cancellation and backend input responses remain available to observers; resident correspondence goes through mail.

Resident tools describe their effects causally:

- `boxen_mailbox` reads this resident's inbox after an optional `after` sequence cursor.
- `boxen_send_message` explicitly queues mail. `to`, nonblank `subject`, and `body` are required. `reply_to` defaults to the sender; `in_reply_to` links a reply. An optional `id` makes retries across different calls idempotent. Retries of the same backend call are also deduplicated through the retained call source.
- `boxen_continuity` continues to manage identity and memory, independently of correspondence.
- `boxen_thread_request` submits an anonymous workload with `request_id`, optional `thread_id`, and `prompt`. Omitted thread ID creates a new thread. Working directory/model default to the resident's saved values. Repeated exact request IDs are idempotent.
- `boxen_thread_result` reads that request's status and eventual ordinary assistant response. It does not create another request or deliver mail. A resident's private narration cannot be retrieved as an anonymous thread response.

Thread requests return immediately and results remain queryable independently. Completion-driven resident activation is not implemented yet; a later activation can read a completed result. Anonymous runtime behavior, steering, streaming, and conventional final response remain unchanged. Mailbox-only instructions and tools are not injected into anonymous threads.

## CLI and client API

```sh
boxen resident-message HOST:42424 PEER_KEY RESIDENT_ID "Hello" --subject "A greeting" --id MESSAGE_ID --client-id CLIENT_ID
boxen mailbox HOST:42424 PEER_KEY RESIDENT_ID
boxen inbox HOST:42424 PEER_KEY --client-id CLIENT_ID --after 0
boxen resident-activate HOST:42424 PEER_KEY RESIDENT_ID --id RUN_ID --directions "..."
boxen resident-update HOST:42424 PEER_KEY RESIDENT_ID EXPECTED_REVISION --auto-activate false
boxen thread-result HOST:42424 PEER_KEY RUN_ID
```

`resident-message` now returns an accepted message, not a run. Its default subject is `Message`; its default client ID is the requesting machine's public key. The GUI retains a random client ID in browser local storage. Another client can use the same explicit ID to share that return queue. The API supports inbox cursor pagination; observers discover activation runs through the existing run catalog and watch them through the same retained event stream. Manual activation returns its run snapshot; repeated activation IDs with the same directions return the original run.

API version 2 advertises the fixed communication contracts in `/api/status`; version-1 custom clients need the resident mailbox operations instead of direct resident starts. Operations use the existing authenticated destination envelope at `POST /api/operation` or the peer protocol:

| Operation | Inputs | Result |
| --- | --- | --- |
| `mailbox_send` | `client_id`, `message`: `id`, `to`, `subject`, `body`, optional `reply_to`, `in_reply_to` | Accepted immutable message |
| `resident_inbox` | Resident `id`, optional `after` | Owner-authorized messages with delivery projection and `next_after` |
| `client_inbox` | `client_id`, optional `after` | Messages addressed to that owner's client namespace, with `next_after` |
| `resident_activate` | Resident `id`, `activation_id`, optional `directions` | Accepted activation snapshot; no incoming mail is required |
| `resident_update` | Existing revisioned patch, now also accepting `auto_activate` | Updated resident |
| `mailbox_status` | None | Availability, journal sequence/count, and any subsystem incident |
| `thread_result` | Anonymous run `id` | Snapshot, terminal `done` flag, and eventual conventional `response` |

A resident can send to another owner-authorized resident on the same host; that message follows the recipient's activation policy. There is no automatic response routing to the original sender. Live execution events describe tool requests/results and private narration; correspondents only receive explicit messages in their client queues.

## Persistence, migration, and recovery

`BOXEN_HOME/mailboxes/events.jsonl` is the single append-only authority for accepted messages and idempotent resident-mailbox registration. It is separate from opt-in diagnostics. Each journal record is synced before publication. Resident profiles own activation policy; run journals own batch association and execution outcomes. No second mutable per-message receipt database is required.

All existing healthy residents are registered on startup without changing their identity or memory. Their historical direct-turn transcripts stay retained private activity and do not become queued or sent messages. On the first mailbox activation of an old resident, Boxen registers the new resident tools in a fresh managed-backend session beneath the same canonical Boxen conversation. A retained `backend_session_rebuilt` event explains the communication migration and bounded history reconstruction. Identity/memory and complete Boxen journals remain intact; hidden backend state is not claimed to transfer losslessly.

Interrupted activations keep their original message association and uncertain outcome. Restart drains only still-queued mail. It does not mark the interrupted batch unread, replay its requests, or require human acknowledgment. The next activation receives the existing recovery context and can inspect earlier mail by its presented run ID. If no new mail is queued, the resident remains available for manual activation or the next message.

An incomplete final mailbox journal record is discarded on recovery because it was never published as accepted. Interior corruption disables the mailbox subsystem, preserves the original ledger, retains a metadata incident in `mailboxes/incidents.jsonl`, and exposes the reason through `mailbox_status`. Anonymous workloads and resident continuity remain accessible; mailbox sends and activations return an explicit error rather than claiming that damaged queues are empty or delivered. Repair/restoration and restart are required for that storage failure. This is not general disk repair or power-loss certification.

Current bounds: subjects are 1–256 nonblank UTF-8 bytes, bodies 1–32768 bytes, and manual directions at most 32768 bytes. An activation selects at most 32 pending messages, with a raw batch budget of roughly 128 KiB and a serialized input budget below the existing 256 KiB prompt limit. A large escaped batch can leave additional messages queued for the next activation. Inbox pages are bounded by 128 messages and roughly 192 KiB of raw content. Eight active workloads per host remains the execution limit. Capacity release notifies the dispatcher. Acceptance does not guarantee successful inference; execution errors remain visible in the activation journal. Workspace repair or configuration changes outside Boxen can require a subsequent message, manual activation, or restart to retry a deferred activation.

Mail, client queues, and activity currently have no retention policy or total disk quota. Resident continuity quota continues to cover only current name/identity/memory. Scheduling, rooms, attachments, cross-machine routing, durable offline outboxes, mobility, and multi-branch resident execution remain outside this slice.

## Verification

`scripts/mailbox-smoke.py` uses real daemons, authenticated transport, and a deterministic backend fixture to check batching, two correspondents, selective explicit replies, private narration, idempotent sends, actor identity, independent activation, distinct anonymous thread tools, default activation, coalescing while active, crash recovery, and escaped payload bounds. Resident, anonymous workload, recovery, diagnostics, and actual APE blocking/CPU checks remain part of verification. On October 31, 2026, the full native and identical-file APE suites passed on macOS ARM64: LAN transport, anonymous workloads, recovery, resident continuity, mailbox delivery, diagnostics, and blocking/idle CPU checks. The mailbox suite additionally verifies explicit resident-to-resident delivery without an automatic reply loop and isolation of a corrupt mailbox journal while ordinary work stays available.

The existing real Codex resident on the Mac mini was migrated with its original identity, memory, canonical Boxen thread, and retained history intact. A mailbox message caused automatic activation, a scoped send tool delivered an explicit reply to its sender's client queue, and a second activation produced private final narration without adding any reply. These two runs retained 122 events. Structured mailbox acceptance and presentation diagnostics were captured with content off. Browser testing then sent a message through the GUI and observed only the explicit reply in **Messages**, while private activity remained separately collapsed. Both physical Macs run the portable binary; other operating systems remain unverified.

## Runtime independence

The [built-in OpenRouter runtime](inference.md) now uses these same daemon-owned operations and communication rules. Resident creation accepts `backend: "openrouter"` (default `"codex"`); the backend is pinned in the resident definition. Anonymous child requests default to that backend. Private narration, explicit sending, incoming-batch presentation, and uncertain-outcome recovery do not depend on which runtime executes the activation.
