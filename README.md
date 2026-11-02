# Boxen

A hospitable home for agents, across your machines.

Boxen brings agent runtimes, inference providers, and granted compute together so you can start a thread, host a named resident, or coordinate work across machines. An agent does not have to earn its place by being useful.

The spirit: **All your box are belong to us.**

Boxen is in early development. Paired LAN machines can execute Codex threads and built-in OpenRouter text/tool and native media-reading workloads with live messages, backend-exposed thinking summaries, tool activity, and command output. Detaching the viewer leaves execution running; reconnecting replays missed events.

## Try the desktop workspace

```sh
python3 scripts/build-desktop.py
# macOS:
open work/Boxen.app
```

The first native Rust client has interchangeable dockable panels, nested splits,
a command palette, and anonymous conversations with VS Code-style preview and
pinned tabs. It attaches to a running daemon and streams retained/live activity.
See [desktop setup, controls and current scope](docs/desktop.md).

## Try the LAN prototype

```sh
cargo build --release
./target/release/boxen identity
./target/release/boxen serve
```

The daemon prints its public identity, LAN listener, and local control-page URL. The plain HTML control page is embedded in the binary; no Node, browser framework, or separate asset installation is required. Open the printed URL to use it. The UI is deliberately minimal while the product experience is being designed. The native Rust desktop client is now available alongside this provisional page; custom GUIs can use the same independent daemon API.

On each machine, trust the other machine's public key, obtained through a trusted channel:

```sh
./target/release/boxen trust OTHER_PUBLIC_KEY "Other box"
./target/release/boxen ping other-machine.lan:42424 OTHER_PUBLIC_KEY "Hello, other box"
```

For the identical-file portable build, run `bash scripts/build-ape.sh`, then use `bash target/cosmo/release/boxen.com` in place of `./target/release/boxen`. End users only need the resulting executable. The first developer build downloads a pinned Rust toolchain and Cosmopolitan compiler.

See [LAN setup, protocol, and verification](docs/p2p.md), including the SSH development deployment helper and the current portability limits.

## Run a remote thread

Install and sign into Codex on the executing machine, pair the two Boxen installations, and start their daemons. Open the local control URL, select the executing machine and its LAN address, connect, enter a destination-local working directory, and send a message. The same page supports follow-ups, steering, cancellation, and detach/reattach. GUI organization remains intentionally provisional.

The CLI exposes the same execution contract:

```sh
boxen start other-machine.lan:42424 OTHER_PUBLIC_KEY /absolute/remote/workspace - "Inspect this workspace"
boxen runs other-machine.lan:42424 OTHER_PUBLIC_KEY
boxen watch other-machine.lan:42424 OTHER_PUBLIC_KEY RUN_ID
boxen cancel other-machine.lan:42424 OTHER_PUBLIC_KEY RUN_ID
```

Use `--thread THREAD_ID` on `start` for a follow-up, `--id RUN_ID` for a retry-safe submission, and `watch --after SEQUENCE` to replay only missed events. `-` selects the executing machine's configured model. `serve --codex PATH` selects its Codex executable explicitly. The selected managed backend must be installed separately; Boxen itself remains one binary.

See [remote workloads and live streaming](docs/workloads.md) for ownership, recovery, and verification, and [the client API](docs/client-api.md) for building an alternative GUI. Shell watchers automatically reconnect after transport loss; `--once` disables retry.

Existing host-local Codex conversations can be [imported](docs/thread-import.md)
through `boxen import-codex`. Import retains visible history and the original
native thread binding without starting inference or creating a resident. [Resident promotion](docs/resident-promotion.md) attaches identity and a mailbox in
a separate explicit operation. GUI import/promotion controls remain separate work.

## Residents

Create a named resident without assigning a job. It has one canonical conversation and a separate revisioned identity/memory store with an enforced configurable quota. It can maintain its own continuity through a daemon-serviced tool. Each resident has a durable mailbox with default automatic activation, batched input, and explicit sending. Final assistant text remains private activity; anonymous threads retain ordinary request/response. Interrupted work leaves a visible incident while the resident stays available for its next message.

[Resident definitions](docs/resident-definitions.md) provide reusable versioned templates and independent instances through the CLI/API. Changes apply to existing instances only through an explicit field-selected manual operation; workload completion leaves instances intact.

See [the first resident](docs/residents.md) for continuity and [mailboxes](docs/mailboxes.md) for communication, activation, tools, CLI/API, and current limits. This first implementation stays on one selected host; synchronization and movement come later.

The [host-local scheduler](docs/scheduling.md) wakes residents and existing anonymous threads once or on a recurring interval. The simple Schedules panel and `boxen schedule` command share the authenticated daemon API; jobs survive client disconnects and daemon restarts.

The [built-in inference runtime](docs/inference.md) now supports OpenRouter text/tool and native media-reading workloads and mailbox residents alongside Codex. Select OpenRouter in the GUI or pass `--backend openrouter` to `start` or `resident-create`. The executing daemon reads its own OpenRouter key from `~/.boxen/credentials/openrouter.key`; credentials are not automatically distributed to peers.

The [compaction research](docs/research/compaction.md) investigates context reduction that preserves original voice, including a required optional mode without summarization. Policy and defaults remain under design.

The [provider/runtime research](docs/research/inference-runtime.md) and [transport experiment](docs/research/inference-transport-experiment.md) explain the boundaries and evidence. [Managed-session portability](docs/research/managed-session-portability.md) remains distinct and unverified.

## Messaging connectors

[Messaging](docs/messaging.md) gives residents a shared inbox/send/status contract
across Boxen mail and external services. Connector instances have separate native
accounts, credential references and explicit resident/conversation bindings. The
bundled Discord runner is `boxen discord-connector --instance ID`; addons can use
the same public intake/outbox protocol without forking the daemon. Private
narration stays private, and uncertain external sends are never automatically
repeated. Discord configuration and opt-in live testing are documented separately
from credential-free regression checks.

## Live daemon diagnostics

Diagnostics are off by default and can be enabled, filtered, streamed remotely, redirected to a file, and disabled without restarting the daemon:

```sh
boxen logs other-machine.lan:42424 OTHER_PUBLIC_KEY --targets runtime,workload | jq .
boxen log-file other-machine.lan:42424 OTHER_PUBLIC_KEY /absolute/path/diagnostics.jsonl
boxen log-status other-machine.lan:42424 OTHER_PUBLIC_KEY
boxen log-off other-machine.lan:42424 OTHER_PUBLIC_KEY
```

See [structured logging and runtime controls](docs/logging.md). [AGENTS.md](AGENTS.md) makes ongoing daemon instrumentation a development requirement. Diagnostic logging is separate from the durable workload journal.

## Development checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
python3 scripts/smoke.py
python3 scripts/workload-smoke.py
python3 scripts/recovery-smoke.py
python3 scripts/resident-smoke.py
python3 scripts/mailbox-smoke.py
python3 scripts/logging-smoke.py
python3 scripts/platform-smoke.py
# After building the portable artifact:
python3 scripts/smoke.py target/cosmo/release/boxen.com
python3 scripts/workload-smoke.py target/cosmo/release/boxen.com
python3 scripts/recovery-smoke.py target/cosmo/release/boxen.com
python3 scripts/resident-smoke.py target/cosmo/release/boxen.com
python3 scripts/mailbox-smoke.py target/cosmo/release/boxen.com
python3 scripts/logging-smoke.py target/cosmo/release/boxen.com
python3 scripts/platform-smoke.py target/cosmo/release/boxen.com
```

Linux is part of the runtime testing rotation. After building with
`--features ape,inference-probe`, run `python3 scripts/test-linux.py` to test the
same portable file on `chad@resident-host.lan`, including isolated regression
suites and a direct cross-host streaming check on Linux TCP port 42426. See [platform testing](docs/testing.md) for
requirements, retained evidence, and verification limits.

Codex also runs headlessly on Linux through the standalone CLI's `app-server`
subcommand; the desktop app and a Node SDK are not required. See
[Linux Codex setup and live checks](docs/workloads.md#headless-codex-on-linux).

## Design

- [Product principles](docs/vision.md)
- [Agreed design decisions and first implementation slice](docs/decisions.md)
- [Problems to solve](docs/design-questions.md)

## Project ancestry

Boxen draws on three existing projects:

- [peer-deploy](https://github.com/x3haloed/peer-deploy): remote execution and machine orchestration.
- [Watch](https://github.com/x3haloed/watch): inference-driven residents, streams, and media.
- [watch-for-business](https://github.com/x3haloed/watch-for-business) (private): a durable daemon, Codex integration, and an attachable companion.

These are sources of experience and capabilities; their architectures are not binding on Boxen.

## License

Dual licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE).

[Observation sources and external adapters](docs/events-runtime.md) provide bounded
JSON batches with optional waking for residents and ordinary threads. USGS and a
small external game are included as [optional adapter examples](examples/adapters/README.md).
