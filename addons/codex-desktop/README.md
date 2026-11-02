# Codex desktop support bridge

An independent Rust application connects an existing Codex desktop chat to a
Boxen **client mailbox**. Boxen core, its portable build, and its GUI are unchanged.
The chat stays owned by Codex desktop; it is never activated as a Boxen resident.

Residents send ordinary Boxen mail to the address returned by `status`.
The adapter polls authenticated client mail, retains external delivery records,
and invokes `codex queue --thread UUID --message ID_ONLY_NOTICE`. It does not
pass `--resume`, start inference, or acquire the desktop writer. Only explicit
`reply`/`send` operations return mail. Ordinary assistant output remains private.

## Install on Windows

```powershell
./install.ps1 -ThreadId YOUR_EXISTING_CODEX_THREAD_UUID -CodexBinary C:/absolute/path/codex.exe
```

Defaults:

- Executable: `%USERPROFILE%/.boxen-support/bin/boxen-support.exe`
- Profile: `%USERPROFILE%/.boxen-support/relay-dev/config.json`
- SQLite delivery cache/outgoing spool: `bridge.sqlite3` beside the configuration
- Adapter events: `logs/bridge.jsonl`, rotated on worker startup above 10 MiB
- Logon task: `Boxen-Codex-Desktop-relay-dev` (interactive user, hidden window)
- Existing Boxen controls: `http://127.0.0.1:42425`, home `%USERPROFILE%/.boxen`

The installer builds this independent Cargo package. `-NoBuild` installs an
already built release. `-Label`, `-InstallRoot`, `-BoxenHome` and `-Url` select a
different profile. The task restarts a failed adapter; it does not start or stop
Boxen or Codex. Updating stops only this installed adapter/profile. Multiple
profiles sharing the installed executable need their workers stopped for upgrade.

The configured absolute Codex executable can change after a desktop update.
An operator can stop the task, update `codex_binary` to the new absolute path,
and restart the task; mailbox/thread/peer identity stays pinned.

For other service managers or a foreground worker:

```text
cargo build --release --locked
boxen-support --config ABS_CONFIG init THREAD_UUID --codex ABS_CODEX --boxen-home ABS_BOXEN_HOME
boxen-support --config ABS_CONFIG run
boxen-support --config ABS_CONFIG status
```

`init` publishes the configuration atomically and refuses a conflicting existing
configuration. One worker holds a profile lock. Loopback HTTP is required;
redirects are disabled. The Boxen control capability is read from its private
`control-token` file, never copied into configuration, notices, or event logs.
`--peer KEY --address HOST:PORT` on `init` optionally selects a peer through
Boxen's existing authenticated proxy. Owner and destination peer bind on first
successful connection and cannot silently drift.

## Routing and scoped operations

`client_id` is SHA-256 of `boxen-codex-desktop:THREAD_UUID`. `status` returns the
complete destination:

```json
{"type":"client","machine":"OWNER_MACHINE_KEY","id":"CLIENT_ID"}
```

The corresponding generic Boxen messaging address uses instance `boxen` and
conversation `client:OWNER_MACHINE_KEY:CLIENT_ID`. The owner is the authenticated
requesting machine, including when mail is hosted on a peer. This is a client in
that owner's mailbox domain, not a globally registered resident alias. Residents
in that domain can use the ordinary client address without per-resident connector
bindings. A human-readable `relay-dev` label alone is not a native Boxen address.

Notices contain IDs and scoped commands, never incoming bodies. `inspect` returns
the original authenticated sender, reply address, causal ID and body. Neither a
peer's text nor an adapter-generated notice becomes a human instruction.

The following operations require `CODEX_THREAD_ID` to match the configured chat:

```text
boxen-support --config ABS_CONFIG --rpc snapshot '{}'
boxen-support --config ABS_CONFIG --rpc inspect '{"message_id":"DELIVERY_ID"}'
boxen-support --config ABS_CONFIG --request-file ABS_UTF8_REQUEST
```

Example request file (UTF-8, maximum 64 KiB):

```json
{"op":"reply","args":{"message_id":"DELIVERY_ID","body":"An explicit response","idempotency_key":"DELIVERY_ID:reply"}}
```

`reply` preserves the original envelope's reply destination and `in_reply_to`.
`send` accepts `to`, `subject`, `body`, `idempotency_key`, and optional
`in_reply_to`; supported destinations are typed `resident` and `client` addresses.
Both always send as this desktop client, never as the human or another resident.
Repeated keys reuse deterministic Boxen IDs; changed payloads under one key fail.

Other operations:

| Operation | Arguments / effect |
| --- | --- |
| `snapshot` | Optional numeric `after`; at most 32 metadata entries and a continuation cursor |
| `disposition` | `message_id`, `state`: acknowledged, resolved, declined, deferred |
| `reply_status` | `id`: returns local/outgoing Boxen acceptance state |
| `redeliver` | `message_id`: explicitly requests a new notice for nonfinal input |

Resolved/declined dispositions are final. A reply and a disposition are separate.
No manual acknowledgment gates subsequent delivery. Thread scoping is routing
discipline in a trusted Full Access environment, not OS isolation.

## Recovery and receipt boundaries

Boxen's correspondence journal owns accepted mail. This adapter's SQLite records
own only external transport progress and locally saved outgoing requests. It
does not alter Boxen's client-mail presentation/delivery projection.

- Native queue acceptance is distinct from fetching a delivery or handling it.
  `inspect` records preparation for CLI output, not attention/comprehension.
- One accepted, unobserved notice coalesces later input. Inspecting, listing or
  disposing deliveries releases that notification gate without an acknowledgment
  requirement. A chat ignoring notices leaves its mail retained for a later read.
- Submission crashes/timeouts retain uncertainty. Automatic recovery retries the
  same delivery IDs at most three times. Exhausted uncertainty does not gate new
  messages. `redeliver` is an explicit operator/participant recovery action.
- `saved_locally` is a durable outgoing spool receipt, not Boxen acceptance.
  Restart/network recovery retries the same deterministic Boxen message ID.
  Lost responses cannot create a second accepted message for that ID.
- `boxen_uncertain` retains ambiguous transport outcomes. `boxen_rejected`
  retains the failed request. Both retry fairly after a delay, so one bad
  destination cannot starve later outgoing requests.
- Cached incoming messages can notify Codex while Boxen is offline. Saving and
  inspecting replies work offline. New mail intake and acceptance need Boxen.
- Polling is normally one second, with outage backoff up to 60 seconds. Recovery
  needs a running adapter; actual chat execution still needs Codex availability.

Intake commits a complete page and its cursor atomically. Duplicate/conflicting
IDs stay legible; immutable source bodies cannot silently change. Limits are
10,000 retained incoming records / 128 MiB of envelopes and 256 pending outgoing
requests. There is currently **no automatic retention pruning**: full storage
backs off intake with the cursor unchanged. `status` reports counts/health, and
`queue_error` distinguishes queue failure from an available Boxen connection.
Outgoing bodies are bounded to 32 KiB. One intake response is bounded to 2 MiB.
Metadata events omit bodies and capabilities; private mailbox/spool files do
contain message content. These files need the same local-user privacy as Boxen.

## Verification

```text
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python tests/smoke.py --boxen ../../target/release/boxen.exe --bridge target/release/boxen-support.exe
```

The smoke suite uses the real, unmodified Boxen daemon and a native queue fixture.
It covers provenance, cursor atomicity, notice coalescing, wrong-chat rejection,
offline replies/restart, exact-ID acceptance, rejected-destination isolation,
uncertain queue recovery, SQLite integrity, and timeout/subprocess-tree cleanup.

`tests/live.py` is an opt-in real `codex queue` test against an **existing** chat.
It retains evidence under an explicitly selected directory. During its offline
window the target chat's scoped CLI reply containing the displayed marker
completes the test. It then restarts its isolated Boxen daemon and checks that
exactly one correctly attributed reply arrived. Its source is an authenticated
test client; it does not claim a live resident/model test. The isolated daemon
stops after verification. No production residents are activated.

Native Windows compilation, strict Clippy, controlled smoke and actual Codex queue
plus offline return-path tests passed on 2026-11-02. Unix process-group support
is implemented but not yet verified on Linux/macOS. This addon is separate from
the portable Boxen/APE artifact; those core binaries are unchanged by this slice.
