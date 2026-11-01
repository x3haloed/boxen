# Built-in inference runtime: first usable slice

Boxen can execute text/tool and native multimodal reading workloads through OpenRouter as well as managed Codex app-server sessions. Both use the daemon's existing paired transport, run IDs, authoritative event journal, discovery, live observation/replay, and controls. Resident communication still uses mailboxes; anonymous threads still return conventional responses.

This is the first built-in runtime, not a claim of universal provider, modality, or native-session portability. The [research](research/inference-runtime.md) and [managed-session portability investigation](research/managed-session-portability.md) remain relevant.

## Configuration and use

The executing machine reads its OpenRouter credential from `~/.boxen/credentials/openrouter.key` by default (relative to the selected daemon data directory). The file contains only the key and should be owner-readable/writable (`600` on Unix). A different path can be selected with `serve --openrouter-key-file PATH`. The credential is read locally at execution, is not included in API submissions or process arguments, and is not copied to a peer. Changing the file's contents takes effect for subsequent requests without a daemon restart. Secret storage and cross-machine sharing are still separate design work; this file convention is not an encrypted credential vault.

The provisional GUI now has an OpenRouter backend option for new anonymous threads and new residents. Existing residents keep their Codex backend. An idle OpenRouter resident can be created before its credential is configured; a failed activation records the credential problem without discarding identity, mail, or history. Later activations can continue after configuration recovers. Their backend is stored in the definition; old definitions without the field decode as Codex. Backend and inference-model changes within an existing thread are rejected in this slice rather than attempting an unverified state conversion.

CLI examples:

```sh
boxen start HOST:42424 PEER_KEY /absolute/destination/cwd deepseek/deepseek-v4.1-flash 'Inspect this directory' --backend openrouter
boxen resident-create HOST:42424 PEER_KEY 'A resident' /absolute/destination/cwd deepseek/deepseek-v4.1-flash --backend openrouter
```

An empty model (CLI `-`) uses `deepseek/deepseek-v4.1-flash` for OpenRouter. Existing Codex defaults remain unchanged. The OpenRouter endpoint is explicitly Chat Completions; it is not treated as an interchangeable format for all providers. `runtime_info` in the authenticated operation API reports runtime kinds, the default model, verified input kinds, and whether the executing host's credential configuration is usable. It does not make a paid request or return a secret. `configured` means a usable credential file or a local test endpoint, not proof of upstream account credit or service availability.

## Ownership and live execution

The daemon owns the loop and every tool dispatch. A disposable transport child runs from the **same executable** for each inference exchange. It performs HTTP/SSE only, reads its configuration through stdin, and returns frames over stdout. It does not own a separate conversation database, execute tools, or deliver messages. The daemon syncs received frames before exposing observations to clients. This boundary makes a blocked HTTP exchange cancellable without a mandatory async runtime or external HTTP executable.

SSE parsing and conservative native-state projection are shared with the earlier [transport experiment](research/inference-transport-experiment.md). The native frames remain in the normal run journal; only a completed, supported projection becomes a durable `inference_message`. Failed-attempt deltas remain visible as partial observations. They are not concatenated into accepted conversation history or mistaken for completion. There are no automatic provider retries or model/route changes initiated by Boxen in this slice.

The GUI and shell watchers receive assistant text, backend-exposed reasoning text, tool-argument fragments, shell output, and status as they occur. Closing a viewer does not stop the daemon's run. Reconnection uses the existing contiguous per-run sequence. Each exchange has its own step number; complete assistant messages reconcile streamed deltas without duplicating the displayed final response.

Reasoning uses the selected model’s provider defaults. A universal reasoning-disable parameter rejects routes without that parameter under strict routing; Boxen does not send it. Returned reasoning text/state is still observed and retained. Real signed-state compatibility across models/routes is not established by this slice; indexed opaque-state fixture round trips are narrower evidence.

## Tools and communication

Every built-in workload has `boxen_history` for exact journal recall and context operations, plus `boxen_shell`, a Full-access shell in its configured working directory. Tools run serially. A complete assistant response and a normal tool-call finish are conditions of dispatch. Calls from length-limited or otherwise incomplete responses are not executed. JSON arguments are parsed and validated by the selected tool; malformed inputs produce errors rather than guessed repairs. A retained dispatch record precedes tool execution, and the result is retained before another inference request.

The shell accepts `command` and optional `timeout_ms` (default 30000, range 1–120000). It returns a combined text output and exit code, with real-time stdout/stderr observations. Output is capped at 256 KiB. Native Unix and the verified macOS APE use an owned process group so cancellation/time limits also terminate ordinary descendants. Files, network requests, detached work, or external effects can already have occurred; termination is not rollback. Windows shell/process-group behavior remains unverified.

Identity-bearing agents additionally receive the same daemon-owned tools used by Codex residents: `boxen_continuity`, `boxen_mailbox`, `boxen_send_message`, `boxen_thread_request`, and `boxen_thread_result`. Sender identity, mailbox ownership, continuity revisions/quotas, and send idempotency are enforced by the existing authorities. An anonymous child request defaults to its resident's selected backend/model. It remains a separate request/response channel, not mail.

Resident assistant text is private execution narration, including final text. Only an explicit successful send delivers correspondence. A resident can selectively reply to a batch, complete without sending, or accept an independent activation without incoming mail. No automatic final-output-to-mail conversion is added by this runtime. Anonymous requests that end without a conventional text response record an explicit failure/uncertain outcome instead of inventing a reply.

## History, limits, and recovery

Accepted native messages and tool results live in the existing run journals. Follow-ups rebuild provider input from these retained records, including the tested native metadata. Daemon restart preserves them; no in-memory SDK session is required. Inference history does not depend on opt-in diagnostics.

A crash or cancellation between an assistant call and its durable result leaves the effect uncertain. A later activation can proceed: the current projection supplies a synthetic unknown result describing that uncertainty. It does not dispatch the historical call or relabel it successful. A completed effect followed by failed inference retains its original result. Explicit mail sends also retain the existing daemon idempotency guarantees.

The built-in runtime now has selectable summary compaction and summary-free verbatim continuity, backed by the same original journals, exact paged history, source bundles, coverage records, and provenance. Anonymous threads default to summary; residents default to verbatim. Policy selection is independent of communication style. Settings, failure behavior, bounds, and verification are documented in [built-in context policies](context.md). The former 512 KiB whole-run projection is retired.

Other initial bounds: 16 inference exchanges per activation, 2048 requested output tokens per exchange, 120 seconds per HTTP request plus a supervisor deadline, 24 MiB outbound request, 1 MiB SSE line/frame, and 16 MiB received exchange data. The host's existing eight-active-run limit remains. Provider accounting, tool schemas, and media budgeting need broader work before these become general-purpose resource policies.

Cancel closes an inference child promptly; upstream processing or billing may already have occurred. Cancel during a shell call records uncertain effects and terminates its owned process group on the verified Unix path. Anonymous steering queues for the next inference boundary, including after a running tool; resident correspondence remains mailbox input for a later activation. Arbitrary backend input-response controls are explicitly rejected because this runtime has no such pending request.

## Diagnostics and verification

New paths emit lazy, structured `runtime` diagnostics under `inference.*`: exchange preparation, frame retention, completion, process teardown, tool dispatch/results, and shell lifecycle. Existing hot filters, remote subscriptions, file sinks, payload opt-in, bounded delivery, and secret redaction remain in effect. Diagnostics are off by default. Content-bearing conversation frames and messages are authoritative retained state, not diagnostic payload capture.

Run the deterministic integration proof with either artifact:

```sh
python3 scripts/inference-runtime-smoke.py target/release/boxen
python3 scripts/inference-runtime-smoke.py target/cosmo/release/boxen.com
```

It starts two real paired daemons and a controlled provider, exercising actual shell effects, streamed observation, replay, native-state follow-up, malformed/truncated calls, uncertain results, cancellation, steering, daemon restart, selective replies, no-send completion, continuity writes, and anonymous child requests. The live counterpart, `scripts/inference-runtime-live.py`, uses the local OpenRouter key and synthetic scratch-only prompts with DeepSeek 4.1 Flash; it verifies shell output, follow-up history, explicit selective mail, and no-send completion. Both native and APE live runs passed on macOS ARM64. Broader existing Codex, mailbox, logging, recovery, and platform tests remain regression checks.

The APE worker-launch test found that Rust's Linux-target `current_exe()` looked for `/proc/self/exe` on macOS. `platform::executable()` now uses Cosmopolitan's `GetProgramExecutableName` when built as APE. The actual worker path and process-group operations are covered by runtime tests, beyond successful compilation alone.

Still planned: client media uploads, general provider adapters/options, model-specific token/window accounting, safe route/model switching, scheduling, and managed-session migration. The [Linux rotation](testing.md) now verifies the built-in runtime and native media projection on Debian 13 x86-64 with controlled providers. Live inference on Linux, Windows, and macOS x86-64 execution remain unverified.

## Development deployment evidence

The development Macs were updated to the identical APE with SHA-256 `926bd715273f5bca6a3a4f0d528482a5c0ef1d8407777ab0b2bf58e1a829ae09` (8,745,128 bytes). A live request submitted and watched from the Mac mini executed on the Mac holding the OpenRouter key: 145 contiguous retained events, observable shell output, a verified scratch-file effect, and a completed conventional response. The key stayed on the executing Mac. Both daemons restarted with diagnostics off; existing resident definitions and journals were retained.

A real headless Chrome session also selected OpenRouter in the provisional GUI, submitted a shell workload, and displayed its streamed activity, command output, and final response without browser errors. The native/APE controlled-provider suites, existing Codex workload/recovery/resident/mailbox/logging checks, 11 unit tests, and actual native/APE blocking/wakeup checks passed. Cross-platform claims remain limited to the macOS ARM64 machines actually exercised.

## Native file reading

Every built-in workload now has `boxen_read` alongside `boxen_shell`. Managed
Codex tools remain unchanged. Reads resolve relative to the executing workload's
working directory (absolute paths also work) and detect types from bytes rather
than filename extensions. The tool provides these inputs without local conversion:

| Artifact | OpenRouter Chat Completions part |
| --- | --- |
| UTF-8 text | Tool result text, with byte offset/limit |
| PNG, JPEG, GIF, WebP | `image_url` with a data URL |
| WAV, MP3, FLAC, Ogg Opus/Vorbis | `input_audio` with base64 and format |
| MP4, QuickTime MOV, WebM | `video_url` with a data URL |
| PDF | `file` with a data URL; `file-parser` engine explicitly `native` |

A PDF is never silently sent through OpenRouter's default OCR fallback. A model
and route that cannot accept the native input can reject the request; catalog
modalities alone do not establish actual compatibility. Container recognition is
not codec validation. Other encodings, non-PDF binary documents, media generation,
live capture, uploads from a client, and page/time selection are not implemented by
this read tool. Explicit external conversions remain possible through Full-access
shell execution, but the reader itself does not transcribe, extract frames, resize,
or summarize. See OpenRouter's primary documentation for [audio](https://openrouter.ai/docs/guides/overview/multimodal/audio),
[video](https://openrouter.ai/docs/guides/overview/multimodal/videos), and
[PDF processing](https://openrouter.ai/docs/guides/overview/multimodal/pdfs).

Text reads accept `offset` (UTF-8 byte boundary) and `limit` (1–65536 bytes,
default 65536). Responses report total bytes, next offset, truncation, and SHA-256
of the bytes read. Files must be regular and at most 2 MiB, including text in this
initial implementation. Media reads send the whole original file and reject text
selection parameters. Unknown binary formats fail explicitly. On Unix a
nonblocking open prevents a FIFO replacement from hanging the reader.

Successful media reads publish a private, synced, content-addressed snapshot at
`runs/RUN_ID/artifacts/SHA256` before retaining its reference in the accepted tool
result. Existing snapshots are integrity-checked. The per-run snapshot quota is
16 MiB, with duplicate bytes counted once. These initial enforced bounds are not
a final configurable retention/replication policy; attachments accumulate across
runs until deliberately removed. Agent portable-storage accounting remains a
separate design decision.

Journals carry small typed references, not base64 media. At each exchange the
projection validates and expands those references into native wire parts. Because
Chat Completions expects these media parts in user content, a clearly labeled
Boxen tool-output message follows the complete group of tool results. Its text
identifies the source tool call; this is not attributed to a human. The canonical
accepted tool result owns the reference, including after interruption before the
next provider request. Internal reference fields never go to the provider.

The wire projection permits at most 16 MiB of expanded source media and a 24 MiB
serialized request. Older run groups still follow the disclosed bounded context
policy; snapshot retention does not imply perpetual prompt inclusion. Snapshot
files and journals together form the retained input and must travel together in
a future migration. No automatic migration or cross-host blob download is added.
Deleting or modifying the original source path does not change retained input.
Missing, oversized, or damaged snapshots produce explicit unavailability notices
and retained `artifact_unavailable` events; subsequent work can continue without
rereading the source or repeating historical tool calls. Diagnostics remain off
by default; read dispatch/results use existing structured correlated tool events,
and failed snapshot projections emit `inference.media.unavailable`.

The provisional GUI displays read activity and snapshot metadata through its
existing tool rows. It does not yet provide media upload or inline previews.

Verification:

```sh
# Controlled provider, two paired daemons, actual file reads and retained snapshots.
python3 scripts/media-smoke.py target/release/boxen
python3 scripts/media-smoke.py target/cosmo/release/boxen.com
# Paid, small synthetic media; requires Pillow, ffmpeg, and macOS say for fixtures.
python3 scripts/media-smoke.py target/release/boxen --live --report work/media-live.json
```

The deterministic proof checks exact native wire bytes, PDF processing mode,
provenance, executor restart, sequence replay, source deletion, damaged-snapshot
recovery, unknown/oversized files, the snapshot quota, and a request above 1 MiB.
Live fixtures use opaque random filenames, random visual properties/color order,
a spoken phrase selected independently of the filename, and a random document
marker. Correct perception is checked alongside actual read-tool execution;
completed text alone does not pass. The fixture dependencies are development-only,
not daemon dependencies. Live model failures remain failures in the report.

### Live evidence, 2026-10-31

Two randomized matrices (native and macOS ARM64 APE, 13 cases each) exercised five
inexpensive models. Files were 12 bytes of text, sub-1-KiB PNGs/PDFs, roughly
70 KiB WAVs, and a 4-second, roughly 2 KiB MP4. No answer-bearing filenames or
prompt hints supplied the expected colors, counts, phrases, order, or random
marker.

| Model | Cases exercised | Observed limitations in these runs |
| --- | --- | --- |
| `google/gemini-2.5-flash-lite` | Text, image, audio, video, native PDF | Refused the native run's image read; omitted the second spoken word in both randomized audio runs |
| `xiaomi/mimo-v2.6-flash` | Image, audio, video | Correct native-run video; wrong APE-run color sequence, despite the correct source bytes |
| `qwen/qwen3.8-omni-flash` | Audio, video | Both kinds passed in both runs |
| `openai/gpt-4.1-nano` | Image, native PDF | Miscounted four shapes as five in the native run; correct on the different APE-run image |
| `deepseek/deepseek-v4.1-flash` | Image | Passed both randomized images |

The native matrix passed 10/13 perception checks; APE passed 11/13. Both scripts
correctly exited nonzero for the failed perception checks. These are small
compatibility probes, not estimates of model accuracy or evidence that one
binary changes perception. Every modality had successful live evidence. A
separate frame decode confirmed the APE-run video's actual green-to-blue order;
Qwen correctly described that same retained file while MiMo described red-to-green.
Model errors and refusals do not trigger hidden provider fallback or conversion.

An earlier parameter probe isolated HTTP 404 from GPT-4.1 Nano when the universal
reasoning-disable parameter was sent with strict routing; otherwise identical
requests without that parameter returned HTTP 200. Removing the override made
both GPT-4.1 Nano media routes execute. The earlier failed probes remain evidence,
not successful compatibility checks.

The two matrices above reported a combined cost of about $0.0071; this excludes
initial investigation and other live regressions. All 13 Rust unit tests, Clippy,
controlled media/runtime suites, native/APE blocking and wake checks, and existing
APE workload/recovery/resident/mailbox/logging suites passed. A separate real APE
DeepSeek regression also passed shell execution, stateful follow-up, selective
mail delivery, and no-send resident completion. The live perception evidence
above used macOS ARM64. Subsequent Linux x86-64 controlled-provider and media-wire
verification is recorded in [the platform rotation](testing.md); Windows and other
OS/architecture combinations remain unverified.

After the final dependency consolidation, the shipping APE passed a fresh selected
five-case live proof: Flash Lite text, GPT-4.1 Nano native PDF, Qwen Omni audio and
video, and DeepSeek Flash image. All five demonstrated the randomized contents
using only `boxen_read` (including two PDF reads requested by Nano). This confirms
the final artifact's input paths; it does not replace or erase the broader
matrices' model failures. That additional proof reported about $0.0020.

Both Macs were upgraded, with no active production workloads, to the identical
8,849,954-byte APE, SHA-256
`4297808e1b802042780d809a815e65741bbc532857eac9dabb95e0f2b1f9ca67`.
A final real LAN proof requested and watched execution from the Mac mini while
the local Mac retained its credential. DeepSeek read an opaque-named PNG,
correctly identified four green squares, and streamed 93 contiguous events back
to the mini. The retained snapshot matched the original bytes. Existing journals,
resident definitions, and control capabilities were retained; diagnostics restarted
off. The GUI continues to show metadata rather than inline media previews.
