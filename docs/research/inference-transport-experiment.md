# Inference transport and replay experiment

This opt-in experiment crosses the first uncertain boundary identified in the [runtime research](inference-runtime.md): the same Rust executable performs an HTTPS streaming exchange, retains provider frames before publication, exits, and reconstructs assistant/tool-call state in a fresh process for a follow-up exchange.

It is **not an inference backend in the daemon**, an autonomous agent loop, a credential-sharing mechanism, or a managed-session migration implementation. Production Codex execution and resident communication remain unchanged. See [managed-session portability](managed-session-portability.md) for that separate investigation.

## Build and run

The `inference-probe` feature uses Ureq 3.5.0 with rustls/ring and bundled WebPKI roots, without a Node runtime, external HTTP executable, gzip, or an async runtime requirement. The experiment commands are not enabled in ordinary builds. The [subsequent daemon runtime](../inference.md) now shares this transport and includes its dependency in normal builds. Dependency selection remains provisional pending broader platform and operational tests.

```sh
cargo build --locked --release --features inference-probe
COSMO_HOME="$PWD/work/cosmo" cargo build --locked --release --features ape,inference-probe
```

The build script explicitly forwards the probe feature to the nested Cosmopolitan builds. Merely enabling a feature on the outer native Cargo invocation did not previously select it for the APE.

For live verification, put the OpenRouter key alone on one line in `~/.boxen/credentials/openrouter.key`, outside the repo, readable only by its owner. The development file was created with mode `600`. The probe reads a key file; the key is not passed as a command-line argument, included in requests retained as conversation data, or copied to another host. This is a development credential convention, not the final credential vault design.

```sh
python3 scripts/inference-probe-smoke.py target/release/boxen
python3 scripts/inference-probe-smoke.py target/cosmo/release/boxen.com
python3 scripts/inference-probe-live.py target/release/boxen
python3 scripts/inference-probe-live.py target/cosmo/release/boxen.com
```

Live tests use `deepseek/deepseek-v4.1-flash`, confirmed in OpenRouter's public [model catalog](https://openrouter.ai/api/v1/models) with tool and tool-choice parameters. They make two bounded synthetic requests, with at most 256 output tokens each, disabled reasoning, and parameter support required. A deterministic echo result is supplied by the test harness; no model-chosen command executes. The helper prints only model, outcome, token count, and reported cost, and removes its temporary journals afterward.

The lower-level CLI is available for inspecting a specific experiment:

```sh
boxen inference-probe --request request.json --journal new-exchange.jsonl --key-file /absolute/path/openrouter.key
boxen inference-replay --journal new-exchange.jsonl
```

The default endpoint is explicitly OpenRouter Chat Completions. Endpoint overrides accept only unauthenticated IPv4 loopback fixtures, including TLS fixtures; a key file with an override is rejected before any request. Redirects are disabled. This prevents accidentally sending the live credential to a test server or redirected host.

## Contract and retained facts

One synchronous CLI invocation owns one exchange. It writes a new, owner-only JSONL journal with contiguous sequence numbers and timestamps. Request data, response status, parsed native provider frames, and explicit exchange completion/failure are retained. Each record is synced before it is emitted on stdout. Journals are authoritative for this experiment, distinct from opt-in daemon diagnostics.

SSE parsing handles comments, multi-line data, CRLF, fragmented network reads and UTF-8, and bounds individual lines/frames to 1 MiB. The journal is capped at 16 MiB, request files at 1 MiB, and total request duration is configurable from 1–300 seconds (120 by default). Content-rich journals are intentionally sensitive experiment artifacts. The production daemon's content-capture and logging rules have not changed.

A finish reason is not sufficient by itself: `[DONE]` must arrive before the exchange is considered complete. Late usage and a repeated finish reason are accepted as accounting. HTTP errors, malformed frames, in-stream errors, oversized input, timeout, and premature EOF leave a failed/incomplete exchange. Redirect/HTTP response bodies and low-level transport setup details are suppressed in public error text; the failure record retains the sanitized reason. Existing journal files are never overwritten.

Replay is a separate process. It validates sequence ordering, ignores an unpublished torn final line, and returns no accepted assistant for incomplete exchanges or unsupported projections. It assembles text/reasoning strings, indexed tool argument fragments, and a **limited tested subset** of indexed reasoning-detail fragments. Unknown consistent delta fields survive; unknown changing fields cannot silently enter accepted context. Native frame JSON remains available even when projection is unsupported. Opaque replay rules beyond the tested shape are unproven; this code is not a universal Chat Completions normalizer.

The live helper validates the completed tool name and exact arguments before constructing an echo tool result. It then sends the retained assistant plus that tool result through another probe process. A length-limited response remains marked `length`; the probe has no tool executor that could act on it.

## Verification envelope

Verified locally on macOS ARM64:

- Native Rust and the actual identical-file APE both pass the adversarial fixture suite.
- Both perform real HTTPS OpenRouter tool-call/follow-up exchanges with DeepSeek 4.1 Flash and fresh-process journal replay.
- Fixtures cover streaming before completion, retention before output, fragmented Unicode and SSE, opaque metadata round-trip, unknown fields, partial tool arguments, late usage/repeated finish, EOF, provider errors, redirects, deadlines, size limits, length stops, unsupported projections, sequence corruption, torn tails, credential origin restrictions, and rejection of an untrusted TLS certificate.
- Existing unit tests, formatting, Clippy, and native/APE blocking/wakeup/idle-CPU platform checks remain part of verification. The workload smoke tests exercise unchanged daemon behavior with the feature present.

The initial successful live pairs reported 442 total tokens each, with costs of approximately $0.0000704 native and $0.0000461 APE. These are observed request accounting, not advertised or fixed model pricing. An earlier one-exchange V4 Flash sanity check preceded selecting the catalog's explicit V4.1 ID.

The feature-bearing APE is approximately 8.4 MB, compared with the preceding daemon APE at approximately 4.7 MB. Both architectures are packaged, but only the ARM64 macOS execution path was run. Windows, Linux, and macOS x86-64 remain unverified for this transport. Platform checks consumed well under 0.1 CPU seconds during approximately 0.46 seconds of blocking/wakeup tests; this is not a provider-throughput benchmark.

Still unproven: native multimodality, model/route switching with real signed state, proxy/custom trust-store behavior, production cancellation, provider retries, tool-effect durability and uncertain-outcome recovery, context budgeting/compaction, daemon integration, remote inference workloads, and resident mailbox behavior on the new runtime. A stdout consumer can terminate this CLI experiment; viewer independence belongs to the existing daemon contract and will be required during integration.

## Next frontier

The transport stack is a viable candidate under the measured envelope, not a final universal choice. The next coherent slice can put one inference exchange and a small validated tool loop under the daemon's existing run journal, control, diagnostics, and mailbox authorities. Native media and provider-state replay need explicit verification before claiming those capabilities. Managed-runtime migration requires its own disposable two-host probe.
