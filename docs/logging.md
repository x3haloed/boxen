# Live daemon diagnostics

Diagnostic logging starts **off on every daemon launch**. There is no diagnostic history collected in the background while it is off. Attach a stream or configure a file sink to enable it on the running process. Remove the last sink or use `log-off` to disable it without restarting. Configuration is intentionally not restored after a restart.

The root [AGENTS.md](../AGENTS.md) makes instrumentation and these guarantees part of ongoing feature development.

## Shell access

Use the existing paired-machine authentication and encrypted LAN connection:

```sh
# Attaching enables diagnostics for this subscription; Ctrl-C detaches.
boxen logs HOST:42424 PEER_KEY

# Narrow high-volume sources before records are constructed or transmitted.
boxen logs HOST:42424 PEER_KEY --level debug --targets runtime,workload
boxen logs HOST:42424 PEER_KEY --targets runtime --events rpc. --run RUN_ID

# Opt into content such as prompts, backend messages, tool arguments and output.
boxen logs HOST:42424 PEER_KEY --targets runtime --payloads | jq .

# Consumer-side queries use fields, without parsing prose.
boxen logs HOST:42424 PEER_KEY --targets runtime |
  jq -c 'select(.event == "rpc.received" and .fields.method == "item/commandExecution/outputDelta")'
```

For the portable build, substitute `bash target/cosmo/release/boxen.com` for `boxen`. The CLI writes one JSON record per stdout line and sends attachment feedback to stderr. No SSH tunnel is needed after pairing. A disconnected subscription is removed automatically; it does not leave a hidden collector running.

Diagnostics are daemon-wide and use the existing paired-operator trust model. Fine-grained per-domain access is future work.

## File output and runtime controls

```sh
# The path is on the daemon's machine and its parent must exist.
boxen log-file HOST:42424 PEER_KEY /absolute/path/diagnostics.jsonl --level trace

# Change the destination, filters, or payload setting while running.
boxen log-file HOST:42424 PEER_KEY /absolute/path/runtime.jsonl --targets runtime --payloads

boxen log-status HOST:42424 PEER_KEY
boxen log-off HOST:42424 PEER_KEY
```

A file sink and multiple stream subscribers can coexist with independent filters and payload settings. Reconfiguring the file replaces only that sink; stream subscriptions continue. File output appends NDJSON. Switching files drains accepted records to the old file before returning. `log-off` stops production, drains the file queue, and closes existing subscriptions after their accepted records. A new subscription can subsequently enable logging again.

File output creates regular files with mode 0600 on Unix. Existing permissions are preserved. File-open/configuration failures are returned to the caller. A later write failure disables the file sink, records `file_error` and loss counters in status, and emits `logging.file_failed` to other matching sinks. There is no automatic rotation or disk-retention policy; redirect to a new path at runtime. Diagnostic files are not fsynced per record and are not authoritative recovery data.

## Filters and schema

Every sink independently selects a minimum severity (`trace`, `debug`, `info`, `warn`, `error`), targets, event-name prefixes, and optional peer/thread/run IDs. Empty target/prefix lists match all. Prefixes within `--events` are ORed; different filter dimensions are ANDed. The default is all targets at trace severity, with payload capture off.

Available targets are `daemon`, `peer`, `http`, `workload`, `runtime`, and `state`. Filters can be supplied with `--level`, `--targets`, `--events`, `--peer`, `--thread`, `--run`, and `--payloads` on either `logs` or `log-file`.

Records use this envelope:

```json
{
  "v": 1,
  "session_id": "daemon-session-id",
  "seq": 125,
  "timestamp_ms": 1793470000000,
  "machine_id": "public-machine-identity",
  "pid": 1234,
  "level": "trace",
  "target": "runtime",
  "event": "rpc.received",
  "context": {"peer_id": "...", "thread_id": "...", "run_id": "..."},
  "fields": {"method": "item/commandExecution/outputDelta", "id": null, "error": false}
}
```

`payload` appears only for sinks explicitly requesting content. Event names are stable identifiers; variable data belongs in `fields`. Correlation keys are present where known. Connection IDs correlate handshakes and transport operations. Backend RPC IDs are fields within the relevant run. Sequence numbers order emitted diagnostics within a daemon session; filtering naturally leaves gaps, so sequence gaps alone do not indicate loss. Restart creates a new session.

Instrumentation covers connection attempts, authentication outcomes, messages and closure; HTTP routing and responses; accepted/rejected operations; workload submission, control, status, retained events and replay; journal creation/recovery; and Codex process lifecycle, RPC traffic, stderr, invalid output and failures. Startup information printed for launching the UI and direct CLI errors remain human-facing command output.

Private machine keys and local HTTP authentication capabilities are never collected. Local HTTP authorization headers and process environment dumps are not captured. Recognized secret fields and bearer/API-key text are redacted before output. Optional payloads still contain application content as entered, including prompts and tool output; redaction is not a general classifier for every secret someone could embed in arbitrary text. Metadata and payload sections are each bounded at 64 KiB; larger sections become explicit truncation records containing original size and limit rather than being silently clipped.

## Performance and consumer failure

The disabled producer path checks the installed logger and an atomic target/severity mask. It does not construct fields or payloads, serialize records, perform diagnostic I/O, or allocate per-event diagnostic records. Fine-grained matching happens before lazy fields are evaluated. Payload construction is skipped unless a matching sink requests it.

Enabled sinks have bounded nonblocking queues, each limited to 256 records and 2 MiB. At most eight sinks, including the file sink, can be active. Serialization and queue admission happen in the producer; disk and network I/O happen in consumers. A slow or disconnected consumer cannot make workload execution wait for its I/O.

Overflow produces a structured `logging.dropped` notice with a drop count and `seq: null`; it also increments per-sink and daemon-wide counters exposed by `log-status`. Notices are delivered when the consumer next reads, not inserted as fabricated events into workload history. Accepted queued records can precede or follow a loss notice; use the count rather than inferring an exact lost range. File errors also remain visible in status.

Log-stream traffic is excluded from per-message diagnostic capture to prevent feedback loops. Connection setup/teardown and configuration operations can still generate a bounded number of records for other active sinks.

## Diagnostics and workload journals

The workload journal stays enabled independently of diagnostic logging. It remains responsible for event retention, viewer replay, idempotency and uncertain-outcome recovery. Turning diagnostics off does not affect those behaviors. Backend stderr is now an opt-in diagnostic source rather than a mandatory workload-journal entry; historical journal entries remain intact.

Diagnostic subscriptions are live-only and have no replay contract. For durable diagnostic retention, configure a file sink or save the CLI stream on the consumer. For authoritative workload replay, use `watch` and its sequence cursor.

## Durable infrastructure history

Each daemon retains metadata for peer connection failures, selected operation attempts and outcomes, and Codex and built-in runtime launches. Tracked operations include workload start and control, resident activation and continuation, thread movement and historical workspace access, and mail. Records contain stable stage, outcome, and reason codes, machine and request correlation, timing, and available runtime and policy versions. Polls, streams, periodic network sync, and bulk transfer chunks stay outside operation tracking. Prompts, paths, credentials, raw errors, and message contents stay outside these records.

The destination records dispatch before attempting its reply. An origin with an unknown reply can match the destination's record by `request_id` when both daemons support the history. A completed dispatch describes the operation's return. For a workload start, the run journal still owns the execution outcome.

Read a daemon's history through the authenticated operator API or an existing paired connection.

```sh
boxen request HOST:PORT PEER_KEY '{"op":"infrastructure_history","after":0}'
```

The response contains up to 128 records in sequence order. Pass `next_after` as the next request's `after` when more pages are available. `oldest_seq`, `newest_seq`, and `history_before_cursor_unavailable` describe the retained range. Sequence gaps, invalid rows, write failures, and recovered partial rows indicate missing evidence. Two 4 MiB segments bound retention per daemon. Records are synced to disk at the recording boundary, and a history write failure allows the operation to continue. This history stays enabled across restarts independently of diagnostic logging.

## Maintaining instrumentation

Use `logging::record` with lazy field/payload closures and the applicable target and context. Runtime code can use `Run::diagnostic` to obtain correlation without cloning run state when logging is off. Never prepare formatted strings or clone payloads before calling the logger.

```rust
logging::record(
    Level::Debug, Target::Peer, "connect.requested", context,
    || json!({"address": address}),
    || Value::Null,
);
```

Instrument decisions and confirmed outcomes at their owning boundary. Keep logging outside the authority of workload journals. New transports and sinks must preserve bounded buffering, filtering, explicit loss reporting, and suppression of logging feedback loops.

## Verification

Rust tests cover the disabled no-op path, lazy category/prefix/run filters, per-sink payload selection, secret-field redaction, bounded slow consumers, explicit loss notices, and file switching/draining. `scripts/logging-smoke.py` runs two actual daemons and verifies default-off behavior, authenticated remote CLI subscription, live enable/filter changes, credential redaction, independent file output, hot redirection, workload/runtime correlation, off without restart, subscriber disconnect cleanup, and reset-to-off after restart. Existing workload smoke tests verify that diagnostic changes do not affect execution and replay.

Run these against both native and APE builds:

```sh
python3 scripts/logging-smoke.py
python3 scripts/logging-smoke.py target/cosmo/release/boxen.com
python3 scripts/platform-smoke.py target/cosmo/release/boxen.com
```

Live diagnostics exposed a portability defect in the original 6.x shim: an idle APE watch generated over 53,000 `events.read` records in six seconds. A native timing probe blocked correctly; the old APE returned from a condition-variable wait 100 times in under a millisecond without notification. The pinned upstream 10.0.0 compatibility shim supplies the missing portable futex translation.

The CPU probe exposed a second issue in Cosmopolitan 4.0.2: the negative 32-bit sentinel used by Rust's channel parker was sign-extended into XNU's 64-bit wait parameter, causing immediate returns. An APE-only `ulock_wait` link wrapper preserves the unsigned 32-bit comparison value for 32-bit operations and leaves 64-bit operations unchanged. This wrapper must not log through the primitives it repairs. Retire it when the pinned compiler fixes the ABI boundary and the probe passes without it. See `src/platform.rs`.

`boxen platform-check` exercises real timeout and notification primitives. The platform smoke test rejects excessive CPU use, and the logging smoke test bounds idle watch activity. Do not replace these checks with filters that merely hide a busy loop.

The identical final APE was also verified on two physical macOS ARM64 machines: a remote shell subscription and metadata-only file sink captured a real Codex run, filters selected the expected records, and hot disable left the daemon PID unchanged. The repeated short run produced 101 file records with no dropped diagnostics, replacing the excessive idle-read activity. Both daemons were left with diagnostics off. The final blocking probe used approximately 0.005 seconds of CPU over approximately 0.47 seconds of wall time on the development Mac; this is a local check, not a cross-platform performance claim.
