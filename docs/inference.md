# Built-in inference runtime: first usable slice

Boxen can execute text/tool workloads through OpenRouter as well as managed Codex app-server sessions. Both use the daemon's existing paired transport, run IDs, authoritative event journal, discovery, live observation/replay, and controls. Resident communication still uses mailboxes; anonymous threads still return conventional responses.

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

Reasoning is requested disabled for this initial model configuration. Returned reasoning text/state is still observed and retained. Real signed-state compatibility across models/routes is not established by this slice; indexed opaque-state fixture round trips are narrower evidence.

## Tools and communication

Every built-in workload has `boxen_shell`, a Full-access shell in its configured working directory. Tools run serially. A complete assistant response and a normal tool-call finish are conditions of dispatch. Calls from length-limited or otherwise incomplete responses are not executed. JSON arguments are parsed and validated by the selected tool; malformed inputs produce errors rather than guessed repairs. A retained dispatch record precedes tool execution, and the result is retained before another inference request.

The shell accepts `command` and optional `timeout_ms` (default 30000, range 1–120000). It returns a combined text output and exit code, with real-time stdout/stderr observations. Output is capped at 256 KiB. Native Unix and the verified macOS APE use an owned process group so cancellation/time limits also terminate ordinary descendants. Files, network requests, detached work, or external effects can already have occurred; termination is not rollback. Windows shell/process-group behavior remains unverified.

Identity-bearing agents additionally receive the same daemon-owned tools used by Codex residents: `boxen_continuity`, `boxen_mailbox`, `boxen_send_message`, `boxen_thread_request`, and `boxen_thread_result`. Sender identity, mailbox ownership, continuity revisions/quotas, and send idempotency are enforced by the existing authorities. An anonymous child request defaults to its resident's selected backend/model. It remains a separate request/response channel, not mail.

Resident assistant text is private execution narration, including final text. Only an explicit successful send delivers correspondence. A resident can selectively reply to a batch, complete without sending, or accept an independent activation without incoming mail. No automatic final-output-to-mail conversion is added by this runtime. Anonymous requests that end without a conventional text response record an explicit failure/uncertain outcome instead of inventing a reply.

## History, limits, and recovery

Accepted native messages and tool results live in the existing run journals. Follow-ups rebuild provider input from these retained records, including the tested native metadata. Daemon restart preserves them; no in-memory SDK session is required. Inference history does not depend on opt-in diagnostics.

A crash or cancellation between an assistant call and its durable result leaves the effect uncertain. A later activation can proceed: the current projection supplies a synthetic unknown result describing that uncertainty. It does not dispatch the historical call or relabel it successful. A completed effect followed by failed inference retains its original result. Explicit mail sends also retain the existing daemon idempotency guarantees.

This initial context policy keeps complete recent run groups within a 512 KiB prior-message budget. It records omitted-message counts and supplies the location of full journals. Identity/memory and the new input are retained independently. This is a disclosed bounded projection, **not model-generated compaction or lossless continuity**. Very large current input/tools or output can still exceed the request budget and end an activation with a retained incident; a later activation stays possible. Automatic summary compaction remains later work.

Other initial bounds: 16 inference exchanges per activation, 2048 requested output tokens per exchange, 120 seconds per HTTP request plus a supervisor deadline, 1 MiB outbound request, 1 MiB SSE line/frame, and 16 MiB received exchange data. The host's existing eight-active-run limit remains. Provider accounting, tool schemas, and media budgeting need broader work before these become general-purpose resource policies.

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

Still planned: native media through the daemon API and tools, general provider adapters/options, model-generated compaction, richer budget configuration, safe route/model switching, scheduling, and managed-session migration. Linux, Windows, and macOS x86-64 execution of this runtime are not yet verified.

## Development deployment evidence

The development Macs were updated to the identical APE with SHA-256 `926bd715273f5bca6a3a4f0d528482a5c0ef1d8407777ab0b2bf58e1a829ae09` (8,745,128 bytes). A live request submitted and watched from the Mac mini executed on the Mac holding the OpenRouter key: 145 contiguous retained events, observable shell output, a verified scratch-file effect, and a completed conventional response. The key stayed on the executing Mac. Both daemons restarted with diagnostics off; existing resident definitions and journals were retained.

A real headless Chrome session also selected OpenRouter in the provisional GUI, submitted a shell workload, and displayed its streamed activity, command output, and final response without browser errors. The native/APE controlled-provider suites, existing Codex workload/recovery/resident/mailbox/logging checks, 11 unit tests, and actual native/APE blocking/wakeup checks passed. Cross-platform claims remain limited to the macOS ARM64 machines actually exercised.
