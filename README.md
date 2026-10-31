# Boxen

A hospitable home for agents, across your machines.

Boxen brings agent runtimes, inference providers, and granted compute together so you can start a thread, host a named resident, or coordinate work across machines. An agent does not have to earn its place by being useful.

The spirit: **All your box are belong to us.**

Boxen is in early development. Paired LAN machines can now execute anonymous Codex threads with live thinking summaries, messages, tool activity, and command output. Detaching the viewer leaves execution running; reconnecting replays missed events.

## Try the LAN prototype

```sh
cargo build --release
./target/release/boxen identity
./target/release/boxen serve
```

The daemon prints its public identity, LAN listener, and local control-page URL. The plain HTML control page is embedded in the binary; no Node, browser framework, or separate asset installation is required. Open the printed URL to use it. The UI is deliberately minimal while the product experience is being designed.

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

See [remote workloads and live streaming](docs/workloads.md) for ownership, recovery, and verification.

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
python3 scripts/logging-smoke.py
python3 scripts/platform-smoke.py
# After building the portable artifact:
python3 scripts/smoke.py target/cosmo/release/boxen.com
python3 scripts/workload-smoke.py target/cosmo/release/boxen.com
python3 scripts/logging-smoke.py target/cosmo/release/boxen.com
python3 scripts/platform-smoke.py target/cosmo/release/boxen.com
```

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
