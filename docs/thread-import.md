# Import an existing Codex thread

Import brings an existing **host-local native Codex thread** into Boxen as an
ordinary identity-less conversation. It is separate from creating or promoting a
resident. There are no new GUI controls in this slice.

```sh
boxen import-codex HOST:42424 PEER_KEY NATIVE_CODEX_THREAD_ID --id BOXEN_ID
boxen runs HOST:42424 PEER_KEY
boxen watch HOST:42424 PEER_KEY BOXEN_ID --once
```

`NATIVE_CODEX_THREAD_ID` is a Codex UUID. `BOXEN_ID` is a new 64-character lowercase
hex ID, retained across retries. The CLI generates one when omitted; an explicit
ID permits exact retries after an uncertain response. The returned snapshot has
`record.spec.thread_id` for the canonical Boxen conversation and `backend_thread`
for its original native Codex thread. IDs are not interchangeable.

The selected executing daemon uses its configured Codex executable and host-local
session store. This operation does not transfer sessions, credentials, attachments,
working files or execution environments from another machine.

## What import retains

Read-only `thread/read` and `thread/turns/list` requests load metadata and full
public turn/item history. Boxen retains a snapshot in its existing run/event
journal, independent of opt-in diagnostics. No resume, fork, turn, historical tool
call or model inference is started by import. It does not seize an external writer.

The import record includes source metadata, native ID, observation/import time,
turn count and SHA-256 of the serialized public turn array. Turn IDs, original
statuses, errors and available timestamps remain in `codex_import_turn` events;
native items remain in historical `backend` events. User text has a historical
`user_message` projection for existing viewers. These are historical data, not new
requests, outgoing mail, newly executed tools or certified original outcomes.
`item/completed` envelopes provide existing-viewer projection; the item itself
retains its source status, including any unknown/interrupted work.

The import's terminal `completed` status means the snapshot was accepted, not that
all source work succeeded. `record.spec.prompt` is empty because import submits no
new user prompt. Its `record.imported` provenance distinguishes it from a model run.
Normal catalog, get and watch operations discover/replay it; reconnecting and
restarting do not reread the source or re-execute historical work.

The snapshot is the public app-server projection. Hidden reasoning, backend
compaction/session internals and referenced attachment bytes are not exported by
this operation. Native execution continuity still belongs to the original session;
the snapshot is not an interchangeable native-session backup or a lossless
cross-host migration artifact. Later external edits are not automatically mirrored.

## Continue the conversation

An explicit ordinary `start` request uses the imported Boxen thread ID and retained
working directory. The import snapshot reports the model when Codex exposes it;
an empty model retains the runtime's normal default/resume behavior.

```sh
boxen start HOST:42424 PEER_KEY /original/working/directory MODEL "New message" \
  --thread BOXEN_ID --id NEW_RUN_ID
```

Boxen resumes the original native thread, preserving its provider selection and
not overriding its saved developer instructions with resident framing. Full Access
remains the execution default. Existing native tool availability is unchanged;
import does not add dynamic tools to an older session. [Resident promotion](resident-promotion.md) adds an identity and mailbox through
a separate explicit operation, preserving the native session through an MCP bridge.

A writer held by Codex desktop or another app-server can reject continuation. A
missing or rejected imported native session fails visibly instead of silently
starting a replacement session. Human-directed repair can happen without losing
the retained snapshot. Importing does not authorize a second simultaneous writer;
the previous execution owner needs to release the native thread before continuation.

## API

The authenticated peer operation and loopback `POST /api/operation` destination
envelope use the existing [client API](client-api.md):

```json
{
  "op": "thread_import",
  "spec": {
    "id": "64_LOWERCASE_HEX_CHARACTERS",
    "backend_thread_id": "NATIVE_CODEX_UUID"
  }
}
```

The result is an ordinary run snapshot with `record.imported` provenance. Repeating
the exact ID/native-ID/owner returns the accepted snapshot without reading or
executing the source again. Authenticated network operators can inspect and retry the same accepted import while original ownership remains retained. Conflicting ID reuse, an ID
already used by a resident, and a native thread already registered in this daemon
are rejected. This ownership guard covers this daemon, not other daemon stores or
external Codex clients. Promotion is a separate [resident operation](resident-promotion.md).

All metadata/history is captured before publication. Complete synced staging files
are renamed into the authoritative run directory together; interrupted staging is
not a run. Unknown acceptance follows the normal exact-ID retry/catalog contract.
Process restart is tested; this does not certify power-loss behavior.

The native history API must support ascending full-item turn pagination. Import
rejects in-progress turns, incomplete item views, repeated turns/cursors, malformed
responses, changing metadata observed during the read, and excessive bounds rather
than accepting an unmarked partial snapshot. Metadata is limited to 256 KiB,
history to 64 MiB/10,000 turns, responses/events to approximately 4 MiB each,
and each native request to 30 seconds. History collection checks a two-minute
deadline between pages. The source does not provide a cross-process snapshot lock;
quiescing the original conversation provides the strongest consistency. The
metadata recheck detects changes but is not an atomic native-store transaction.

## Verification

```sh
python3 scripts/thread-import-smoke.py target/release/boxen
python3 scripts/thread-import-smoke.py target/cosmo/release/boxen.com
```

The controlled suite uses real paired daemons and the explicit test backend. It
covers metadata/history pagination, historical item retention, no implicit
execution, bounds/failures without partial acceptance, ownership, exact retries,
replay/restart, same-native-thread follow-up and refusal without reconstruction.
The Linux rotation includes this suite. Windows uses a test-only command wrapper
for Python; production still uses the configured native Codex executable.

An opt-in live proof uses existing local sign-in and two tiny real model turns:

```sh
python3 scripts/thread-import-live.py target/release/boxen \
  --codex /absolute/path/to/codex --model YOUR_AVAILABLE_MODEL
```

It seeds a disposable native session, imports its visible history without a turn,
and verifies a subsequent Boxen request recalls a random earlier marker on the
same native thread. Evidence stays under ignored `work/thread-import-live/`; the
test stops its processes and changes no production resident. It consumes model
usage. Native Codex session records remain in the host's existing store.

### November 2, 2026 evidence

On Windows x86-64, all 27 Rust unit tests, strict all-target Clippy, the native
release build, import smoke suite and native blocking/notification check passed.
The pre-existing simulated Linux discovery test now compares paths as paths,
avoiding a Windows separator-only assertion failure.

The controlled import suite also verified concurrent exact-ID retries, recovery
after an interrupted follow-up without replacing native developer instructions,
and metadata-only diagnostic output independent of retained history.

An actual Codex CLI `0.166.0-alpha.5.1` / `gpt-6-luna` test captured a disposable
native thread's original user/assistant messages, then recalled its random marker
through Boxen on the same native thread without reconstruction. That temporary
native chat was archived afterward; no production agent was imported or promoted.

APE/macOS/Linux checks were not run for this slice from the Windows checkout.
The authorized Linux host was unreachable through trusted SSH because its host key
was not trusted in this host's SSH configuration; verification did not bypass that
check. The updated Linux runner includes the import suite for that rotation.

The native protocol follows the official [app-server thread read and history
APIs](https://learn.chatgpt.com/docs/app-server#read-a-stored-thread-without-resuming).
