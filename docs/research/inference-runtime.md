# Inference providers and the Boxen runtime

Research date: 2026-10-31. This is an evidence-backed design recommendation, not an implementation contract or a claim of provider compatibility. Existing product decisions in [decisions](../decisions.md) remain authoritative.

## Recommendation

Boxen should own the built-in agent loop, durable conversation, tool execution lifecycle, context projection, and communication semantics. Provider libraries can implement inference exchanges underneath that ownership. A library's agent abstraction should not become the definition of a Boxen agent.

Do not adopt `aisdk-rust` as the canonical conversation format or built-in runtime in its inspected form. Its Rust implementation is attractive for packaging, but the inspected types and conversions impose precisely the information loss we need to avoid. This is a judgment about this revision, not every possible lower-level use of the library.

Treat Vercel AI SDK, Pi, Codex, and the current OpenCode inference package as complementary references. Vercel offers a rich provider contract and a large body of compatibility work; Pi makes conversion and loop boundaries particularly visible; Codex demonstrates transport, tool, and context lifecycle detail; OpenCode now offers another substantial provider implementation and explicit native-state escape hatches. None supplies Boxen's complete product semantics.

The promising implementation direction is a small Rust runtime with explicit protocol adapters and serializable provider state, initially for one selected OpenRouter API. That recommendation does **not** mean “implement every provider ourselves immediately.” First prove the storage and execution contract against adversarial recorded exchanges. Then evaluate whether an existing Rust transport/SDK can sit below it without losing information or breaking the portable executable. Do not choose the HTTP/TLS stack on source inspection alone.

## Evidence and limits

All seven requested local checkouts were clean when inspected. They were not fetched or updated. These are their recorded heads, not assertions that they are the latest upstream release:

| Checkout | Revision | Commit date | Evidence used |
| --- | --- | --- | --- |
| `Research/ai` | `dffe4053c15709577329518044b1ec1d4c6bb0fe` | 2026-10-30 | Provider v4 types, provider converters and tests, stream loop/retry contract |
| `Research/aisdk-rust` | `f6fe5253dd415a3bacb96041a7007c51eb3acff9` | 2026-03-23 | Core messages/capabilities/extensions, streaming loop, OpenAI Responses and Chat conversions, dependencies |
| `Research/codex` | `67f0994b532b8740f14b5b738af7b7b2a915b1d6` | 2026-10-31 | Responses SSE, session client, tool admission/cancellation, history normalization and compaction |
| `Research/opencode` | `dc4775a64da024bc94f385c85e344d03005e7410` | 2026-10-31 | `packages/ai`, protocol converters, tool history, session transport/retry/compaction |
| `Research/pi` | `6af92c7950e8e8aaf79248159132f827c7a8b1a6` | 2026-10-31 | Coding-agent session/compaction, agent loop, AI message types and compatibility conversion |
| `Research/claude-code` | `8f12b112ca1edc733e2660c4e4f43e340a83b94e` | 2025-05-19 | Unpacked `@anthropic-ai/claude-code` 0.2.122, bundled CLI, package/license; current official SDK docs separately |
| `watch` | `cd823453423891698057a417f6e35ac4c72fe5b1` | 2026-10-24 | Model registry, request transforms, history/media repairs, loop checkpoints, forensics, existing tests |

Source references below are pinned to those revisions. Watch and the unpacked Claude repository may require the reader's GitHub access. Current official documentation was checked separately where protocol behavior matters. Documentation already differs from older checkouts, which is itself a reason to version adapters and recorded fixtures.

This was a targeted source investigation, not an exhaustive audit of every provider. No paid/live inference requests, SDK build comparisons, binary-size benchmarks, or APE networking experiments were performed. Existing Watch tests were executed; other projects' tests were inspected where relevant but not run. Claude's bundle contains dependencies as well as application code, so finding a string there is not sufficient evidence of a particular application execution path.

## 1. What breaks in Watch

Watch is the most relevant evidence because it actually exercises our opinions: continuous residents, optional outward communication, media, model switching, interruption, and persistent context. These are concrete compatibility measures, not proof that every current SDK version has the same defect.

### Media survives the product but not the serializer

Watch represents media in SDK message/file structures, then converts OpenRouter and Novita audio/video into specially prefixed text parts. A request-body transform turns those sentinels back into native `video_url` or `input_audio` parts after the generic compatible provider has serialized the request. This is a working escape route through an abstraction that cannot express that path directly. [W1] [W2]

Tool-result media has a second boundary: the compatible Chat provider serializes the tool content as JSON text. Watch keeps a textual tool result to satisfy call/result pairing, then adds the real media in a synthetic user message, explicitly identifying its originating tool call. It also recognizes legacy persisted shapes and SDK 7 file shapes. [W1]

**Implication:** “supports video” cannot be a boolean on a model alone. Capability depends on model, upstream endpoint, selected API, adapter, content location (user/tool/assistant), media representation, and options. A tool returning video must remain video through storage, projection, wire encoding, and the next model call.

### A model switch must not destroy the source material

`messagesForModel` clones history before replacing unsupported media and applying provider-specific conversions. `prepareStep` rebuilds each step from untransformed history. The existing agent test verifies that switching back from a text-only model restores tool images. [W1] [W3] [W4]

**Implication:** canonical history and the current model's input are different representations. A temporary placeholder is an input projection, not a reason to erase an attachment from history. Boxen should record the projection loss and retain the original media reference.

### Completed effects outlive failed inference

Watch collects completed-step response messages in `onStepEnd`, then preserves those checkpoints on interruption. Otherwise the next activation could forget an already completed tool call. Its tests exercise both ordinary persistence and interruption. [W3] [W4]

**Implication:** one final `generate()` return value is too late to establish durable execution history. Boxen needs checkpoints during the loop and must not automatically repeat completed effects when a later provider exchange fails.

### Repair is already runtime policy

Watch repairs orphaned tool calls with explicit unknown results, inserts a synthetic unknown call for orphaned results, and repairs certain malformed argument shapes, including a `params` wrapper or a scalar for a single required property. These are semantic decisions above HTTP compatibility. [W1]

**Implication:** repairs must be named, bounded, and visible. A synthetic unknown call is a protocol accommodation, not evidence that such a call was observed. Broadly “helpful” argument repair can change an action's meaning. Preserve original arguments and validation failure; initially return a causal tool error for reissuance rather than guessing arguments for an effectful operation.

### Provider diagnostics need the final wire representation

Watch's request recorder captures the transformed request body with sequence, model/provider metadata, hash, and bounded retention. Capturing only the pre-SDK request would miss the sentinel conversion and serializer behavior. Error classification also retains provider status and response details. [W2] [W5]

**Implication:** Boxen needs optional, redacted diagnostics at the actual transport boundary as well as normalized event metadata. Preserve the current off-by-default/hot-enable logging contract; do not copy Watch's request-content capture defaults. Required conversation state remains authoritative even with diagnostics off.

## 2. Provider library findings

### Vercel AI SDK

**Strengths.** The inspected provider v4 prompt is substantially richer than a text chat transcript: ordered text/file/reasoning/reasoning-file/tool/custom content, role-specific combinations, provider options, and multiple file representations including provider references. It explicitly expects runtime capability validation. This is useful design evidence against inventing a universal `role + string`. [V1]

Its Google converter and tests deal with thought signatures, provider-option namespaces across Google/Vertex/gateway paths, and parallel calls where only the first call is signed. The Anthropic converter carries reasoning signatures and provider metadata, and handles provider-specific content constraints. These are examples of valuable compatibility engineering that a hand-written generic JSON adapter would otherwise rediscover. [V2] [V3]

**Where opinions enter.** `streamText` is more than transport: it owns steps, tool execution callbacks, stopping, tool-call repair, and retry behavior. Its explicit stream-retry contract says already emitted partial output cannot be retracted, but is excluded from the recovered step result, response messages, and future model steps. Earlier completed steps remain preserved. Consequently a transcript built from emitted deltas can differ from a transcript built from the returned response messages. [V4]

**Boxen fit.** Excellent protocol/reference corpus. A possible optional process adapter if justified later. Directly adopting the TypeScript runtime would introduce a mandatory execution dependency contrary to the self-contained daemon direction. More importantly, SDK messages, UI messages, provider items, and Boxen conversation events must not become interchangeable simply because the SDK can convert between them. Provider metadata is necessary but still needs a durable, versioned replay contract in Boxen.

### `aisdk-rust` / `lazy-hq/aisdk` 0.5.2

This is an independent Rust library inspired by Vercel, not a Rust distribution of Vercel's complete provider implementation. Its package identifies the project, version, feature set, Tokio runtime, and Reqwest/rustls transport dependencies. Many named provider features reuse its Chat Completions implementation. Provider count alone is therefore not protocol or capability coverage. [R1]

Concrete blockers in the inspected implementation:

- `UserMessage` is a single `String`. `AssistantMessage` has one `LanguageModelResponseContentType`, whose variants are text, tool call, reasoning, and not-supported. That cannot directly preserve arbitrary ordered multimodal assistant blocks. [R2] [R3]
- Chat wire messages use `content: Option<String>`, not native multimodal content arrays. [R4]
- Chat replay converts reasoning into ordinary assistant text with a `[Reasoning]:` prefix. That changes its meaning and does not preserve structured signed reasoning. [R5]
- Responses replay reconstructs a reasoning item from its readable summary and explicitly sets `id`, `content`, and `encrypted_content` to `None`, ignoring the reasoning extensions in that conversion. That path is insufficient for opaque reasoning continuation. [R6]
- `Extensions` stores `Any` values keyed by Rust `TypeId` behind a shared lock. It is an in-process extension mechanism, not a serializable portable-history format. Clone shares the backing map; it is not an isolated durable snapshot. [R7]
- Compile-time capability markers include video/audio/image, but the dynamic-model macro implements all capabilities. This bypasses static checking; it is not evidence that the model, encoder, or route can carry those modalities. [R8]
- The high-level streaming method spawns a tool loop and executes tools when tool-call completions arrive. Its control/checkpoint decisions would need to be reconciled with Boxen's journal and cancellation rules rather than silently delegated to it. [R9]

The library does offer raw body/header overrides, lower-level model interfaces, and extension points. Those deserve credit. They do not by themselves repair the core history and response conversions above. We have not established that every provider path loses the same metadata, nor that its dependency stack cannot build as APE; those would require separate experiments.

**Recommendation:** do not make this library the foundation by accepting these losses and accumulating post-serialization patches. If reconsidered, first demonstrate native media, signed/opaque replay after process restart, control over tool dispatch and cancellation, unknown-field preservation, and actual APE behavior below a Boxen-owned history format.

### An additional provider reference inside OpenCode

The inspected OpenCode has `packages/ai`, a schema-first Effect implementation. It distinguishes a single exchange (`stream`) from an automatic tool loop (`loop`), exposes raw provider frames before derived events, and provides protocol-specific APIs. Its schema includes media assets, per-part provider metadata, provider-executed tools, and message-level native data. [O1] [O2]

Its Gemini converter retains signatures on the specific reasoning, text, or function-call parts that carried them. Its Responses converter preserves encrypted reasoning in provider metadata. Its tool-history normalizer explicitly inserts missing results, delays intervening system updates, and distinguishes provider-executed tools from local calls. [O3] [O4] [O5]

**Boxen fit:** this is now both a runtime reference and an additional inference-library reference. Learn from the distinction between semantic messages, native fields, protocol lowering, and transport. Do not assume it eliminates semantic policy: for example, its documented structured-output path uses a hidden tool, and its history normalization creates synthetic results. Those choices need deliberate treatment in Boxen. [O1]

## 3. Runtime findings

### Codex

Codex's client separates session-scoped state from per-turn transport state. It caches WebSocket connections and continuation state, and retains session-scoped HTTP fallback behavior. This demonstrates why a response ID or open socket is not a portable conversation identity. [C1]

The Responses SSE implementation recognizes terminal response events and treats EOF before completion as an error. A stream can deliver useful items without proving that the exchange completed. It also preserves provider-specific failure and rate-limit detail rather than relying only on HTTP success. [C2]

Tool dispatch captures the step context in which a tool was advertised. Parallel-capable tools acquire shared admission; exclusive tools acquire exclusive admission. Cancellation distinguishes dispatch/admission from execution, and tests cover cancellation after a handler has completed without rewriting the completed outcome. [C3]

History normalization synthesizes missing outputs and uses stable synthetic IDs to avoid cache invalidation. History and compaction code explicitly distinguish retained information from the model-visible window and defer some installation until successful completion. [C4] [C5] [C6]

**Take:** explicit response lifecycle, advertised-tool snapshots, stable derived identifiers, effect-aware cancellation, and atomic context replacement.

**Do not inherit automatically:** OpenAI Responses as the universal wire model, Codex-specific prompts and policies, tool execution while streaming without a separately reviewed admission rule, or the assumption that its synthetic `aborted` outputs establish what happened in Boxen. Our unknown outcomes must stay unknown. Keep using app-server as a managed runtime; building a second internal tool loop around it would duplicate ownership.

### Pi, especially `packages/coding-agent`

Pi separates provider conversion, the agent loop, and coding-agent session management. The loop has explicit context transformation/conversion, request preparation, steering and follow-up queues, tool execution events, and finish-turn decisions. These are good boundaries for Boxen's own runtime, even though the user-facing product semantics differ. [P1]

The loop declines to execute tool calls when a response ends at the output limit, because arguments may be truncated. It supports sequential and parallel tool execution paths. This is an especially useful correctness choice for a first runtime; it gives up speculative latency to avoid executing an incomplete request. [P1]

The AI layer records API/provider/model provenance, response IDs, visible thinking versus opaque thinking signatures, tool signatures, and raw stop reasons. Its cross-model transform strips incompatible thinking/signatures, normalizes call IDs with a mapping applied to results, retains signed empty blocks on compatible replay, and synthesizes missing tool results. [P2] [P3]

The coding-agent session manager maintains an append-only tree with entry/parent IDs, branch selection, and compaction-aware projection. Compaction retains a recent tail and summarizes an earlier region; summary generation has explicit failure/retry handling. The agent-session layer separately coordinates persistence, extension hooks, compaction, and retry. [P4] [P5] [P6]

**Take:** a context projection over durable history, provenance-aware conversion, explicit steering boundaries, tool argument completeness checks, and first-class compaction entries. This is probably the clearest reference for the shape of our small loop.

**Limits:** the inspected ordinary user-message content union is text/image, and assistant content is text/thinking/tool calls. Other Pi modality APIs exist, but this conversation shape does not itself meet Boxen's native audio/video requirement. Its blanket same-model replay check also should not become our universal rule: compatibility is protocol-specific, and current Google Interactions documentation explicitly allows model switching while retaining thought blocks. [P2] [P3] [D3]

A session tree is not shared resident memory, clone semantics, or branch reintegration. Pi's mechanisms inform those features without settling Boxen's identity model.

### OpenCode

The current session WebSocket transport tracks delivery as send-attempted, provider-observed, or terminal; transport errors distinguish ambiguous delivery from accepted delivery. It manages connection affinity, checkpoints, idle timeouts, rotation, and fallback. Its timeout comment recognizes that reasoning can produce no visible content for minutes. [O6]

Session retry is an explicit policy with published retry-scheduled events and plugin hooks. Compaction distinguishes native replacement windows from summaries, supports background summary work, and applies budget/failure handling. [O7] [O8]

**Take:** delivery-phase errors, transport affinity, explicit retry status, and a clear distinction between native compaction and an ordinary summary. These are more useful than a universal `retryable: true` flag.

**Do not inherit automatically:** the full Effect/service architecture, background compaction complexity in the first slice, coding-focused prompts, or any silent downgrade to a weaker protocol. A fallback is a recorded decision and must preserve the requested capability or explain why it cannot.

### Claude Code

The supplied checkout is an approximately 10 MB bundled JavaScript CLI, version 0.2.122, from May 2025. It exposes print/JSON/stream-JSON output and continue/resume flags; the bundle also contains cache-control and thinking-signature handling. Its license file identifies Anthropic's proprietary terms. This is historical behavioral evidence, not a current, cleanly separable runtime source library to embed or copy. [A1] [A2] [A3]

Current official Agent SDK documentation says normal output delivers completed content blocks; partial-message streaming must be enabled for incremental raw stream events. Those events accompany complete messages, so an adapter must reconcile them rather than append both. The documentation also says subagent token deltas are not forwarded through this stream. That is a concrete observability limit, not something Boxen can fill in by promising local-feeling output universally. [D4]

**Take:** use the managed runtime boundary and negotiate/report exposed capabilities. Future Claude integration needs a real protocol probe covering partial output, tools, custom mailbox tools, session resume, cancellation, and duplicate reconciliation. The old unpacked bundle cannot establish today's behavior or interchangeability with Codex app-server. We did not execute it or extract code for reuse.

## 4. Boundaries Boxen should own

These are recommendations for discussion and implementation, not new approved policies.

### A. Durable history, provider replay state, and current context

Use one authoritative retained conversation with three explicit views:

1. **Observed history:** accepted input, authored output, reasoning summaries exposed by the backend, tool requests/results, incidents, attachment references, and reconstruction/compaction events with provenance.
2. **Provider continuation state:** serializable, versioned native blocks and handles with API/provider/model/route provenance and scope. Some data is replayable only on compatible routes; some is server-owned and unavailable elsewhere. Opaque state remains opaque and is not converted into ordinary narration.
3. **Current input projection:** a derived model window, including summaries, media representation choices, protocol-required synthetic entries, ID mappings, and omitted/incompatible blocks with reasons.

These need not be three databases or three full copies. A journal can retain source events and provider payload references; checkpoints can point into it. Avoid persisting every increasingly long request as authoritative history. Capture request bodies only as bounded opt-in diagnostics.

Canonical history must not be the SDK's mutable message vector or a concatenation of GUI deltas. Migration can preserve observed meaning without preserving hidden state. A move to another machine and a move to another provider have different preconditions: machine-local files, provider file handles, server sessions, credentials, and native replay compatibility all need independent treatment.

### B. Two runtime kinds; one product contract

The managed adapter owns interaction with the external runtime; the built-in runtime owns inference exchanges and local tools. Above both, Boxen owns identity, activation, mailbox delivery, placement, retained events, observation, and recovery.

A provider adapter should perform an inference exchange and return events, completed items, native continuation data, usage, and structured outcomes. It must not silently execute Boxen tools, send mail, compact canonical history, or decide what counts as a resident reply.

Keep exchange completion, activation completion, and communication delivery separate. A resident can complete with private narration or no narration and no outgoing messages. An anonymous request keeps its response contract; refusal, empty output, tool-only completion, length limits, and provider failure need explicit outcomes rather than invented assistant replies. No generic tool-loop stopping rule should force mail delivery.

### C. Capabilities belong to the complete path

Describe selected API and route explicitly rather than “OpenAI-compatible.” Capability discovery should carry supported/unsupported/unknown, provenance, and overrides, with operation-specific validation. Unknown is not synonymous with false or with safe support.

The path includes input/output modality, role placement, MIME/encoding/size/duration limits, URL/file/inline support, hosted tools, tool schemas, structured output, thinking settings, context/output limits, and streaming behavior. Router fallbacks may change that path. Current OpenRouter docs expose `require_parameters` and configurable fallbacks; video support also varies by API and endpoint. [D1] [D2]

For example, “MiMo video through this OpenRouter route” needs an actual model/endpoint selection and a wire-level probe, not a model catalog flag. No MiMo route was tested in this research.

Unsupported data should produce a typed incompatibility or a disclosed projection choice. It should not silently become text, dropped content, sampled frames, or a transcript. Preserve native audio/video when the selected path supports it. Media conversions can be useful options, but they must have provenance and retain the original.

### D. Streaming is a state machine with attempts

Retain request/step/attempt IDs, provider response IDs, item/block IDs, and deltas with start/update/end/terminal state. Handle late usage, duplicate terminal markers, midstream errors after HTTP 200, comments/keepalives, fragmented UTF-8/SSE/JSON, unknown events, and abrupt EOF. An accounting frame must not finalize an activation twice.

Normalized events serve the GUI and stable APIs; versioned native events/state preserve extension and replay fidelity. Unknown provider data can be retained within bounds without being interpreted as a tool or successful completion. A malformed known event affecting control flow is a distinct failure, not a silently ignored extension.

A current concrete example: OpenRouter documents a Chat Completions usage frame that repeats the finish reason, unlike OpenAI's empty-choices usage frame. Its Responses and Messages endpoints terminate differently again. This belongs in protocol fixtures, not a universal rule that every non-null finish reason ends the activation. [D6]

Retries receive new attempt identities. Visible partial output from a failed attempt stays observable and labeled; accepted conversation projection must not concatenate it with replacement output. Provider-event reconciliation is separate from client reconnect replay, which already has Boxen journal sequence numbers. [V4]

### E. Tools need an effect-aware lifecycle

Start conservatively: assemble a complete call, confirm that the exchange's termination permits execution, validate against the advertised tool snapshot, durably record dispatch intent, execute, and retain outcome. Partial argument previews may stream immediately without granting execution.

Represent at least requested, rejected, not-started, started, completed, failed, and outcome-unknown. Call IDs are scoped by run/step/attempt/provider; an upstream ID alone is not a cross-provider idempotency key. Keep wire ID rewrites as reversible mappings.

Cancellation before dispatch differs from cancellation during execution or after completion. Closing an inference connection does not undo a file write, shell process, or delivered message. A crash after a tool effect but before its durable result is an honest uncertainty; it must not trigger blind replay. Idempotent Boxen operations such as mail sends can use their existing keys; arbitrary shell effects do not acquire exactly-once guarantees from a journal.

Default serial execution is a reasonable first implementation. Later parallelism needs explicit tool conflict/admission rules and deterministic result projection, not merely a provider's `parallel_tool_calls` option. Provider-hosted tools need separate attribution and must never be executed again locally.

### F. Retry classification needs both cause and phase

Keep status/code, provider request ID, retry hint, delivery phase, observed output, completed local tools, and cancellation reason. Distinguish auth, rate limit, overload, context overflow, unsupported options, malformed requests, content policy, network failure, and incomplete provider output.

A retryable transport failure does not imply the entire activation is retryable. Bound retries and honor provider delays where applicable. Never let SDK retries multiply daemon retries invisibly. Silent rerouting is particularly dangerous when it discards reasoning, media, privacy settings, or model identity. Availability means continuing honestly with known completed effects and an incident record, not claiming success or requiring manual certification.

### G. Compaction is a context transaction

The built-in runtime needs compaction; managed runtimes retain ownership of their native context mechanisms. Portable reconstruction remains a Boxen responsibility in either case.

A built-in compaction should name the source range and revision, preserve a coherent recent tail and call/result groups, reserve output/tool budget, retain original history, and install a replacement only after a complete usable result. A failed summary must leave the prior context intact. A shorter fallback projection can keep work available with an explicit incident; repeated impossible compaction must end or defer the activation clearly rather than spin.

Keep identity/memory separate from conversation summary. Preserve important opaque blocks according to the protocol; a summary is not a cryptographic replacement. Do not apply one rule to every API: current OpenRouter reasoning requires structured replay; Google's Interactions and GenerateContent place and manage signatures differently. [D3] [D5]

Use reported token counts as evidence, not perfect forecasts. Tool schemas, media, reasoning, output reservation, and provider accounting all matter. Record estimation uncertainty. Exact budget defaults and agent-authored versus system-generated compaction remain design choices.

### H. Extension, credentials, diagnostics, and footprint

Version adapter identifiers, state schemas, tool contracts, and capability observations. Additive provider options and native events should be possible without changing the entire conversation schema. Raw overrides cannot bypass mailbox identity, daemon authorization, or journal ownership. In-process Rust traits are implementation boundaries, not by themselves a safe third-party plugin ABI. External adapters may be an optional extension path, with their installation cost explicit.

Credentials resolve at execution on the authorized host. Durable state records credential references rather than values. Provider uploads and continuation handles may be credential-, region-, or account-scoped. Portability cannot assume that synchronizing an agent should synchronize every credential or all provider data automatically.

Logging covers request preparation, capability decisions, serialization, connect/first event, stream lifecycle, tool admission/results, retry, cancellation, compaction, and recovery, correlated with run/step/attempt/provider. Capture content only through the existing explicit option, redact secrets, and bound buffering. Distinguish required sensitive continuation data from optional diagnostics and protect its storage accordingly.

The native and APE dependency experiment must measure TLS/DNS/proxy/certificate behavior, streaming, blocking/wakeup/idle CPU, memory, and binary growth. A Rust crate is not automatically portable just because the daemon is Rust. No conclusion about Tokio/Reqwest/other stack compatibility follows without that experiment.

## 5. Acceptance matrix before calling the built-in runtime usable

The fixtures should exercise the actual daemon/provider/tool/journal boundary with a controllable local provider server. Recorded fixtures supplement small real-provider probes; neither substitutes for the other.

| Case | Required observation |
| --- | --- |
| Ordinary remote anonymous request | Streamed activity, explicit completion, stable final response, reconnect without duplicate presentation |
| Resident batch with selective replies | Only explicit sends deliver; other assistant text remains private |
| Resident completes without sending | Successful activation; no fabricated reply or forced extra turn |
| Tool-only or empty model response | Explicit outcome consistent with resident/anonymous contract; no infinite continuation loop |
| Native media from user and tool | Actual audio/video/image content reaches the selected supported wire field; original attachment retained |
| Switch to incapable model, then back | Original media remains recoverable; projection changes are recorded |
| Signed/encrypted multi-step reasoning | Exact required native data survives process restart and replays in correct order and scope |
| Model/API/route change | Compatible state retained; incompatible state handled explicitly without rewriting source history |
| Interleaved/fragmented tool arguments | No execution from partial or invalid arguments; stable association with the completed call |
| Length-limited response containing a call | No execution of a potentially truncated call |
| Parallel requests and conflicting tools | Advertised execution policy honored; results associated correctly despite completion order |
| Midstream error after partial narration | Failed attempt remains legible; retry does not duplicate accepted content or completed effects |
| Error after successful local tool | Tool outcome remains in history; next inference does not reexecute it |
| Crash after dispatch before result retention | Unknown outcome retained; subsequent activation available without certification gate |
| Cancel before/during/after tool completion | Distinct retained states; completed result does not become an invented abort |
| SSE comments, duplicate finish, late usage, EOF | No crash/double finalization; missing terminal data remains incomplete |
| Unknown native field/event | Bounded retention and extension visibility; no invented semantics |
| Context overflow/failed summary | Prior window remains intact; bounded recovery with an incident |
| Slow observer and diagnostics consumer | Model/tool progress independent of UI; journal replay and bounded diagnostic loss behavior preserved |
| Move with media/native handles | Required local bytes ready; expired or account-scoped handles do not masquerade as portable data |
| Native and actual APE transport | Real HTTPS exchange, streaming, cancellation, certificate checks, idle CPU and resource measurements |

Watch already supplies useful behavioral vectors for media restoration, transforms, and completed-step checkpoints. Preserve their expected outcomes, not their sentinels as a new Boxen format. OpenCode has recorded provider fixtures and lifecycle tests; Vercel and Pi have signature/compatibility tests worth adapting with attribution rather than assuming their union of policies is correct for Boxen. [W4] [O9] [V2]

## 6. Suggested next slice and unsettled choices

First, agree on a minimal retained exchange/tool lifecycle and a context projection that can preserve opaque native fields and attachment references. Avoid designing an entire framework before executing a request.

Then build a narrow transport experiment against a local adversarial streaming server and a small real OpenRouter probe, running the same path under native Rust and the actual APE. Evaluate the library/transport choice with those results. Select **one explicit OpenRouter API** initially, document its verified capability subset, and keep unsupported operations explicit. The research does not justify making Chat Completions or Responses a permanent universal format; both appear in current OpenRouter documentation.

The first completed built-in runtime slice should prove one anonymous remote workload and the existing resident mailbox contract through the same inference adapter: a real local tool effect, streamed observations, retained continuation, selective explicit sends, no-send completion, and recovery after interruption. A minimal shell/file tool set is sufficient. Shared Boxen mailbox/continuity/thread operations should reuse their existing authority and idempotency rules.

Questions still requiring a deliberate choice:

- Which OpenRouter API/model/route is the first verified target, including a native multimodal probe?
- Which HTTPS/SSE stack actually satisfies APE portability and resource budgets?
- What is the default policy for incompatible reasoning on a model or route switch, with protocol-specific exceptions?
- What exact continuation/content data is retained, encrypted, replicated, budgeted, or expired?
- How does the initial compaction policy combine system recovery with an agent's own continuity practices?
- What activation/step/output budgets are defaults, and what causal status is exposed when they end execution?

These are implementation/design choices, not reasons to ask the user again about already settled mailbox semantics, causal language, Full access, or availability after uncertainty.

## Verification performed

Executed in the existing Watch checkout:

```sh
./node_modules/.bin/tsx --test tests/model-registry.test.ts tests/prompt-builder.test.ts tests/lookout-agent.test.ts
```

Result: **22 passed, 0 failed**, including completed tool-step persistence, interrupted-step checkpoint preservation, image restoration after a model switch, and native request-body media transforms. The available Node was `v25.9.0`; Watch declares Node 24, so this is a bounded behavioral check under the available interpreter, not certification of its supported deployment environment. Tests use fixtures/local test behavior, not live provider compatibility.

No Boxen runtime code or live daemon configuration changed during this research.

## Sources

References point to specific inspected files; relevant symbols and boundaries are described in the text above.

[V1]: https://github.com/vercel/ai/blob/dffe4053c15709577329518044b1ec1d4c6bb0fe/packages/provider/src/language-model/v4/language-model-v4-prompt.ts

[V2]: https://github.com/vercel/ai/blob/dffe4053c15709577329518044b1ec1d4c6bb0fe/packages/google/src/convert-to-google-messages.ts

[V3]: https://github.com/vercel/ai/blob/dffe4053c15709577329518044b1ec1d4c6bb0fe/packages/anthropic/src/convert-to-anthropic-prompt.ts

[V4]: https://github.com/vercel/ai/blob/dffe4053c15709577329518044b1ec1d4c6bb0fe/packages/ai/src/generate-text/stream-text.ts

[R1]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/Cargo.toml

[R2]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/core/messages.rs

[R3]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/core/language_model/mod.rs

[R4]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/providers/openai_chat_completions/client/types.rs

[R5]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/providers/openai_chat_completions/conversions.rs

[R6]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/providers/openai/conversions.rs

[R7]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/extensions.rs

[R8]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/core/capabilities.rs

[R9]: https://github.com/lazy-hq/aisdk/blob/f6fe5253dd415a3bacb96041a7007c51eb3acff9/src/core/language_model/stream_text.rs

[C1]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/client.rs

[C2]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/codex-api/src/sse/responses.rs

[C3]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/tools/parallel.rs

[C4]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/context_manager/normalize.rs

[C5]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/context_manager/history.rs

[C6]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/compact.rs

[O1]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/ai/README.md

[O2]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/ai/src/schema/messages.ts

[O3]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/ai/src/protocols/gemini.ts

[O4]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/ai/src/protocols/open-responses.ts

[O5]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/ai/src/tool-history.ts

[O6]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/core/src/session/model-transport.ts

[O7]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/core/src/session/runner/retry.ts

[O8]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/core/src/session/compaction.ts

[O9]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/ai/test/provider/open-responses-lifecycle.test.ts

[P1]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/agent/src/agent-loop.ts

[P2]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/ai/src/types.ts

[P3]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/ai/src/api/transform-messages.ts

[P4]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/src/core/session-manager.ts

[P5]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/src/core/compaction/compaction.ts

[P6]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/src/core/agent-session.ts

[A1]: https://github.com/x3haloed/claude-code-upstream/blob/8f12b112ca1edc733e2660c4e4f43e340a83b94e/package.json

[A2]: https://github.com/x3haloed/claude-code-upstream/blob/8f12b112ca1edc733e2660c4e4f43e340a83b94e/cli.js

[A3]: https://github.com/x3haloed/claude-code-upstream/blob/8f12b112ca1edc733e2660c4e4f43e340a83b94e/LICENSE.md

[W1]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/src/lookout-helpers.ts

[W2]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/src/model-registry.ts

[W3]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/src/lookout.ts

[W4]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/tests/lookout-agent.test.ts

[W5]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/src/inference-forensics.ts

[D1]: https://openrouter.ai/docs/guides/routing/provider-selection
[D2]: https://openrouter.ai/docs/guides/overview/multimodal/videos
[D3]: https://ai.google.dev/gemini-api/docs/thinking
[D4]: https://code.claude.com/docs/en/agent-sdk/streaming-output
[D5]: https://openrouter.ai/docs/guides/best-practices/reasoning-tokens
[D6]: https://openrouter.ai/docs/api_reference/streaming
