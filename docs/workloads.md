# Remote workloads and live streaming

An anonymous Boxen thread can execute through Codex app-server on a paired LAN machine. Its thinking summaries, agent text, tool lifecycle, command output, file changes, backend input requests, and completion state stream through the local daemon into the browser. Local execution uses the same run/event contract and renderer.

The executing daemon owns the run. It retains each ordered event before publishing it. Viewer connections subscribe to that sequence and do not own execution lifetime. Streaming uses condition-variable notification rather than periodic progress polling; quiet connections receive heartbeat batches. Backend-exposed summaries are shown when available, without inventing thinking text when the backend supplies none.

## Usage

Start each daemon with `boxen serve`. Use `--codex /absolute/path/to/codex` or `BOXEN_CODEX` when needed. Boxen can locate the bundled ChatGPT macOS CLI and common Apple Silicon npm installations; other installations can use `codex` on PATH or an explicit path. The executing machine uses its own existing Codex sign-in and configuration. Credentials are not copied from the requesting machine.

In the local control page, select a machine, enter its LAN address, and connect. Supply an absolute directory that exists on that machine, select Codex and a model (or leave the model at its configured default), and send a message. After completion, sending another message resumes the same backend conversation with a new Boxen run ID. During execution, sending a message requests steering instead. The working directory stays fixed for that thread.

**Cancel run** requests `turn/interrupt`; **Detach viewer** stops observation only. Refreshing the page reconnects to the selected conversation and reconstructs its transcript from retained events. A lost connection is shown as an unknown outcome with reconnect attempts, never as success. The layout is a minimal capability surface for later user-directed UX design.

CLI equivalents:

```sh
boxen start HOST:42424 PEER_KEY /absolute/workspace MODEL "Message"
boxen start HOST:42424 PEER_KEY /absolute/workspace MODEL "Follow-up" --thread THREAD_ID
boxen runs HOST:42424 PEER_KEY
boxen watch HOST:42424 PEER_KEY RUN_ID --after 0
boxen steer HOST:42424 PEER_KEY RUN_ID "Additional direction"
boxen cancel HOST:42424 PEER_KEY RUN_ID
boxen respond HOST:42424 PEER_KEY RUN_ID REQUEST_ID_JSON RESULT_JSON
```

`watch` prints each event as JSON as it arrives. It automatically reconnects with exponential backoff (0.5–5 seconds) and resumes after the last event written to stdout; connection notices go to stderr. Use `--once` to exit on transport failure. Explicit server rejections terminate rather than retrying forever. Both the GUI and CLI retain backend event payloads through the same protocol. Structured backend questions and common command/file approvals have simple controls; other server requests expose their payload and a JSON response field. Full access remains the default, with `approvalPolicy: never` and the user as approval reviewer; Boxen does not silently auto-answer server requests that still occur.

## Identities and authority

Each submission has a client-generated run ID and a Boxen thread ID. The first run normally uses its own ID as the thread ID. Backend thread IDs are separately retained. A completed run stays immutable while follow-up runs resume its backend conversation. Only one run per thread may execute at a time.

Repeating the same run ID, owner, and exact input returns the existing record. Reusing an ID with different input is rejected. The GUI retains an uncertain submission's original input and ID for retry, including across refresh, and distinguishes a confirmed rejection from a connection failure. CLI callers can supply `--id` to obtain the same behavior.

The authenticated submitting machine owns API access to its runs. Other paired identities cannot list, observe, or control those runs through the workload API. Pairing currently also authorizes starting full-access workloads under the destination daemon's OS account; API ownership is not OS-level isolation between mutually trusted peers. Fine-grained grants and domain segregation remain later work.

## Retention and recovery

Executing-machine state lives under `BOXEN_HOME/runs/RUN_ID/`: immutable `record.json` and append-only `events.jsonl`. Files use the private data directory's Unix permissions. Events contain sequence number, timestamp, kind, and payload. Snapshot status and backend thread references are projections of the journal, not a second mutable authority. A filesystem lock prevents two daemons from owning the same runs directory.

Watchers request events after a sequence cursor. The daemon replays the backlog and then waits for new events. A reconnecting viewer discards already-applied events and detects sequence gaps. Transport fragmentation preserves large JSON events and Unicode content across Noise frames. Authoritative completed backend items replace their interim rendered text.

Closing a viewer or losing the client connection leaves the executing daemon and runtime active. Completed results remain available across daemon restart. On restart, a previously unfinished run becomes `interrupted` with an explicitly uncertain outcome; it is not retried. An incomplete final journal write is discarded; interior corruption is rejected. A follow-up after interrupted execution retains the old uncertain outcome and records a recovery notice. It accepts the new message without requiring manual acknowledgment or retrying the old work. Previously issued tool side effects may have occurred even when a result was not observed.

A confirmed backend `interrupted` turn after a cancellation request becomes `cancelled`. If cancellation is not confirmed within 15 seconds, the run becomes uncertain and the app-server process is stopped. Stopping an app-server is not a general rollback or guarantee that every external side effect can be stopped. Runtime transport failure during execution also produces an uncertain outcome.

The provisional viewer retries initial connection, catalog discovery, and event observation. A 12-second stream silence deadline detects stalled connections; quiet healthy watches send two-second heartbeat batches. It keeps rendered history during disconnection, deduplicates by run/sequence, and disables execution controls until replay catches up. Closing and reopening the page reconstructs the view from the executing machine's journal. A two-second idle catalog refresh discovers follow-up runs made by another client, including in the selected thread; active progress still uses push streaming. Switching machines, threads, or detaching cancels the old observer and suppresses stale asynchronous updates. Detaching disables controls until reattached.

The requesting daemon can restart without restarting remote execution. Its local API capability persists in `control-token`; clients with that capability reconnect without an authorization ceremony. An explicitly revoked capability produces instructions to reopen the printed control URL, rather than endless retries. Losing remote contact does not convert a known completed result into a failed run.

The daemon starts one app-server per active run and stops it when the run ends. Idle threads therefore retain records without keeping a model/runtime process resident. Follow-ups start an app-server and resume the recorded backend thread.

## Current envelope

This slice supports explicitly placed text workloads, destination-local files, Codex, and a trusted LAN. Prompts/steering are bounded at 256 KiB, backend event lines at 4 MiB, peer JSON messages at 8 MiB, and active runs at eight per machine. The local UI and peer handlers are bounded independently. A bounded backend-reader queue applies backpressure rather than dropping events.

Journals and in-memory history have no retention policy yet. Named residents now layer a canonical conversation and separate durable continuity over this run contract; see [residents](residents.md). There is no workspace transfer, attachment transport, scheduler, automatic failover, shared memory synchronization, or built-in inference runtime. Event retention is local to the executing machine; the viewer does not provide an offline replica. Power-loss durability and migration of backend-owned sessions are outside the verification envelope.

## Verification

Verified on October 31, 2026 using native macOS ARM64 builds and the identical-file Cosmopolitan APE build:

- Rust tests exercise journal-tail recovery, event sequence preservation, single-daemon ownership, cursor validation, and cross-owner rejection in addition to the LAN authentication tests.
- `scripts/workload-smoke.py` runs two actual daemons with the explicitly test-only `tests/fake-codex.py` backend. It verifies live summary/tool output before completion, duplicate submission handling, steering, large Unicode frames, detach/replay without gaps, follow-up conversation continuity, backend input responses, cancellation, API ownership, daemon restart recovery, and the local execution path. These deterministic tests do not establish model behavior.
- `scripts/recovery-smoke.py` cuts real TCP connections between two daemons and verifies automatic shell reconnection with exact cursors, initially unreachable hosts, lost submission acknowledgements followed by requesting-daemon restart, retained local authorization, snapshot ownership, and executing-daemon crash uncertainty without retry. Native and APE variants pass. The browser was exercised through partition, requesting-daemon restart, pending-input recovery, reopen/replay, and automatic discovery of a follow-up submitted by an independent HTTP client with the deterministic backend.
- A real Codex terminal workload on the Mac mini survived a local daemon restart and resumed at the retained cursor, producing 53 contiguous events and a completed result. Repeating its original submission returned that same completed run. Both physical Macs ran matching APE bytes. This establishes requesting-daemon recovery with the real runtime; executing-daemon crash semantics were exercised with the deterministic backend.
- The APE launched real Codex app-server on the remote Mac mini using its existing sign-in. Codex CLI `0.166.0-alpha.5.1` and model `gpt-6.1-sol` executed a command in the selected remote directory, produced output over several seconds, wrote a proof file there, and reported its contents. Output arrived locally before command/turn completion, followed by streamed response text and confirmed success.
- In the actual browser UI, a follow-up resumed that conversation and rendered an exposed thinking summary, commentary, tool output, and final text. A subsequent timed command visibly streamed while running. The viewer detached, refreshed, recovered the conversation, and continued showing new output. Local cancellation produced a backend-confirmed cancelled state, and the test process was no longer present on the executing machine.
- Formatting, unit tests, Clippy with warnings denied, native/APE transport smoke tests, and native/APE workload smoke tests are the development checks for this slice.

The implementation follows the [official Codex app-server protocol](https://developers.openai.com/codex/app-server), with the installed runtime's generated schema used to check parameter shapes. Reasoning and tool events are backend-dependent; uniform transport does not create capabilities absent from the selected runtime. The [Linux rotation](testing.md) now checks the same APE on Debian 13 x86-64 with the controlled Codex fixture; a real Codex installation on Linux remains unverified. Windows, other untested OS/architecture combinations, and fresh-machine installation remain open.
