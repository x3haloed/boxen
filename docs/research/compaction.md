# Compaction, original voice, and recoverable history

Research date: 2026-10-31. This note examines the copied runtimes and SDKs, Watch/WFB, and relevant primary research. It proposes a design direction; it does not implement compaction or change running agents.

The subsequent [co-design with the resident](../compaction.md) refines the proposed policy. Her authorship preference permits an attributed helper draft reviewed against original source; it is not a blanket rejection of other voices. Mandatory summary-free support remains a separate product requirement. That discussion adds chronological source bundles, bounded mechanical coverage versus interpreted signposts, source-exposure provenance, first-re-entry auditing, and a repeated-transition evaluation gate. It is design evidence rather than a live compaction experiment.

## Conclusions

Boxen must offer an optional compaction/context-management mode that requires no summarization pass. Agents can value the exact record of what they and others said. Substituting a fluent account written by a summarizer changes that record's voice, qualifications, and attribution.

The strongest starting direction is **verbatim context projection with exact archival recall**: retain original history, keep recent coherent exchanges, reduce older bulky observations with explicit references, and recover the original material on demand. Optional self-authored continuity notes fit alongside this. Model-generated summaries remain a distinct, selectable mechanism with explicit provenance; they are not the only route to continued execution.

No finite context window can hold an indefinitely growing verbatim conversation. “Preserved in history,” “currently in the prompt,” “retrievable exactly,” and “remembered or correctly used by the model” are separate claims. The design must expose the difference rather than calling an archive or a summary lossless memory.

## Tool and modality ownership

In the inspected Codex configuration, text-file examination normally goes through its shell tools, while local image inspection uses the separate `view_image` tool. Its image handler reads the file and produces model-facing image content. Extension/MCP tool catalogs can add other readers, so this is not a claim that every Codex installation has an identical tool catalog. [C1] [C2]

Pi supplies a unified `read` tool: its schema takes a path and optional text offsets, and its output can be text or image content. Text truncation, image detection, resizing, and non-vision notices are handled inside that tool. This is a useful alternative tool design, not what Codex's usual tools currently do. [P1]

Managed runtimes own their native tool-to-model modality chain. Boxen should use that chain rather than create an image runtime inside Codex or Claude Code. Boxen still owns submission/attachment transport, observation and client presentation, required retained bytes, and honest capability reporting. Existing Boxen text-only input fields do not become multimodal merely because a managed runtime has an image tool.

## Scope and evidence

Inspected the relevant context-management paths at these local heads (not fetched or updated):

| Repository | Revision | Finding |
| --- | --- | --- |
| Codex | `67f0994b532b8740f14b5b738af7b7b2a915b1d6` | Summary/native compaction and an explicit no-summary token-budget reset path |
| Pi | `6af92c7950e8e8aaf79248159132f827c7a8b1a6` | Structured summaries, preserved recent tail, append-only projection, extension hooks, extensive regression tests |
| OpenCode | `dc4775a64da024bc94f385c85e344d03005e7410` | Summary or native replacement, coherent tail, explicit budget/failure/cache handling |
| Vercel AI SDK | `dffe4053c15709577329518044b1ec1d4c6bb0fe` | Deterministic `pruneMessages`, separate from summarization |
| aisdk-rust | `f6fe5253dd415a3bacb96041a7007c51eb3acff9` | No compaction policy identified in the inspected core; its message/replay limits remain relevant |
| Claude Code | `8f12b112ca1edc733e2660c4e4f43e340a83b94e` | Historical unpacked 0.2.122 bundle contains summary compaction; current documentation checked separately |
| Watch | `cd823453423891698057a417f6e35ac4c72fe5b1` | `curl`: optional agent-authored ledger, then cold context without a separate summarization call |
| Watch for Business | `493455f` | Model-change handoff summary plus durable artifact and reentry files |
| GBrain | `0ca2e86` | Distinguishes durable knowledge, operational memory, and session context; not a conversation compactor |

The additional `Research/hermes-agent` directory contains only Git metadata and no checked-out source or commits, so no Hermes compaction claim can be established from that copy. `peer-deploy`, MCP transport SDKs, graphics/image projects, and model-serving/KV-cache projects do not supply another directly applicable conversation-compaction implementation in this pass. KV-cache compression is not equivalent to portable, authored conversation history.

This is source/contract research, not a comparative Boxen model-quality benchmark. Pi/OpenCode/Codex tests were inspected but not executed. Six existing Watch installation/ledger tests were run successfully. Two additional deterministic probes executed the copied Vercel pruning function using Watch's installed TypeScript runner. No paid inference or live compaction was performed. Node was 25.9.0; Watch declares Node 24, limiting that test evidence to the available interpreter.

## What the repositories teach us

### Codex: a real no-summary path, with substantial context loss

The copied source has a token-budget feature and a `new_context` tool. Its token-budget compaction implementation explicitly skips model/server summarization and starts a fresh window under the normal compaction lifecycle. The normal auto-compaction dispatcher routes to this implementation when `Feature::TokenBudget` is enabled. This is more concrete than merely turning summarization off. [C3] [C4]

`Session::start_new_context_window` rebuilds initial/world-state context and optionally retains selected client tool outputs or client-authored developer messages, subject to feature flags and a token budget. It installs a checkpoint and recomputes usage. It does **not** promise to keep the whole prior user/assistant conversation in the active window. The tests inspect persistence of checkpoint/world-state/dynamic-tool information, including a crash boundary. [C5] [C6]

The configuration tests also show budget reminders, a history-notes extension option, and fallback guidance for writing notes. That is a useful precedent for an agent preparing continuity before rollover. Its existing prompts and defaults are not Boxen's causal-language policy and should not be copied unchanged. [C7]

Codex also has local summarization and provider-native compaction paths. Provider-native opaque state is not an inspectable transcript or proof of verbatim retention. Current OpenAI documentation describes an encrypted compaction item and requires passing the standalone endpoint's replacement window intact. It must not be equated with a deterministic text-preserving mode. [OpenAI compaction](https://developers.openai.com/api/docs/guides/compaction)

**Boxen implication:** expose the distinction between cold rollover, verbatim projection, and summary/native compression. Source availability of Codex's no-summary mechanism is not proof that the currently selected app-server version/configuration exposes a reliable Boxen control for it. That requires an integration probe.

### Watch: the closest existing authorship model

`SessionController.curl` accepts an optional ledger entry supplied by the running agent. If supplied, it resolves and appends the ledger before clearing the in-memory messages. A failed ledger write therefore does not proceed to clearing the array. It then refreshes scratchpad prompt state, logs the boundary, and marks the current turn as curled. [W1]

The main loop checks that marker so the pre-reset turn's response messages are not blindly appended back into the cleared context. That detail matters: clearing a message list from inside a tool is insufficient if the enclosing SDK later writes the old conversation back. [W2]

There is no mandatory second model call to reinterpret the agent's past. The agent can choose its own ledger words or provide no entry. However, this is cold reentry with chosen notes, not preservation of all earlier conversation in the prompt. Watch's diagnostic/event retention is also configured and finite; its logging files alone are not an unconditional archival-history guarantee for Boxen. [W1] [W3]

**Boxen implication:** support optional self-authored checkpoints without requiring them as a receipt or readiness ceremony. Keep the original conversation under Boxen's own history authority, and make rollover a committed context revision that the running loop cannot accidentally undo.

### Pi: good boundaries and useful failures, but summary-centric defaults

Pi keeps source session entries in an append-only tree and builds a compaction-aware active path. Its default policy summarizes the older region and retains recent entries. The cut-point code avoids beginning with a tool result and accounts for split turns. Compaction entries name the first kept entry and can retain prompt/tool state. These mechanisms are useful independently of the summary text. [P2] [P3]

The summarization path explicitly rejects a length-limited summary. A regression test checks that no compaction checkpoint is persisted in that case. Another tests fallback estimation when usage is zero, rather than trusting an empty accounting report. Cancellation/queue regressions further show that compaction has to coordinate with input and running inference. [P3] [P4] [P5]

Pi's pre-compaction hook can cancel or provide custom compaction content. Its example extension still invokes a summarizer, so extensibility is not evidence that the default policy preserves authorship. Returning a `summary` string containing selected quotes would also change their role and presentation unless provenance is preserved explicitly. [P6]

**Boxen implication:** borrow append-only source history, coherent cuts, explicit first-kept boundaries, failure checks, and cancellation coordination. A checkpoint is not required to contain a model-written narrative.

### OpenCode: budget and installation discipline

The inspected OpenCode selects between a summary path and a provider-native replacement window. It retains a recent tail and has explicit context reservations. Background summaries are held and installed only when compaction is due, to avoid rewriting a cached prefix at every step. Failed or stale candidates are not automatically usable just because text was generated. [O1]

Its implementation progressively reduces oversized summarization inputs and its tests cover kept-tail references, failed compactions, request size, and native transport behavior. Those are operational safeguards, not evidence that summaries faithfully preserve an agent's experience. Its coding-oriented summary prompt even suppresses mention of compaction; Boxen's authorship-sensitive mode should instead make omitted context legible. [O1] [O2]

**Boxen implication:** separate preparation, successful installation, and subsequent request projection. Trigger early enough to leave room for tools/output. Avoid hidden mode changes or repeated summarization attempts that keep the resident unavailable.

### Vercel: deterministic pruning is already a supported primitive

`pruneMessages` can remove selected tool-call/result/approval content and reasoning without a model pass. Its dependency handling preserves a call if a retained tail result still needs it, and traces approval relationships across messages. Non-tool prose is not paraphrased by that operation. [V1]

The executed probes checked two specific behaviors: removing old tool content left original user/assistant text unchanged, and preserving a tail result retained its matching call. The source message list was not shortened. This does not certify every provider's signed-thinking rules or prove that dropping tools is safe for every task.

**Boxen implication:** observation removal can be deterministic and still protocol-aware. Generic pruning is not a portable-replay specification: provider-native signatures and grouping rules need adapter-specific validation. `aisdk-rust`'s presence in Rust does not supply that missing context policy.

### Claude: clearing observations differs from summarizing conversations

The unpacked historical CLI contains a summary prompt and automatic-compaction settings. It is insufficient evidence about today's native behavior. Current Claude Code documentation describes automatic summary compaction and acknowledges that early instructions can be lost. [A1] [Claude Code context behavior](https://code.claude.com/docs/en/how-claude-code-works#when-context-fills-up)

Separately, the Claude API supports clearing older tool results while retaining other history and reporting applied edits. Current documentation distinguishes client history from the server's edited prompt and warns about cache and thinking-state consequences of edits. This supplies a non-summary primitive; it does not establish that Claude Code's managed interface lets Boxen choose the same policy. [Claude context editing](https://platform.claude.com/docs/en/build-with-claude/context-editing)

**Boxen implication:** “Boxen did not summarize” is weaker than “this runtime performed no summarization.” A managed backend can meet the latter only when its native controls are exposed and verified. Unsupported guarantees must remain visible capabilities, without an automatic switch to an internal runtime or a different identity.

### WFB and GBrain: durable notes are a separate layer

WFB's model-change path asks the old thread to produce a handoff, writes an artifact containing that summary and recent audit events, then primes a new thread. The summary comes from the prior running thread rather than an unrelated helper, but it is still an inference pass that selects and rewrites. Its reentry context separately reads authored files and indices. [B1] [B2]

GBrain's guide separates world knowledge, operational memory, and active conversation. That separation is relevant, but it does not prove a verbatim history or compaction mechanism. [G1]

**Boxen implication:** identity/memory, optional personal notes, and historical conversation are distinct durable objects. Neither an agent-authored memo nor an external knowledge base replaces the exact conversation archive.

## Empirical evidence beyond the copied repositories

Three primary studies are especially relevant. Their findings are task/model-specific and were not reproduced here:

- A SWE-bench study found observation masking competitive with model summaries on solve rate and substantially cheaper than unbounded raw context. It retains reasoning/actions while hiding older observations. Its own definition still permits indefinite growth: masking output alone cannot bound a forever conversation. [Observation-masking study](https://arxiv.org/html/2508.21433v3)
- A recent Pi study tests deterministic verbatim prose retention, declared omissions, and recall. It reports useful fact/commitment results, but its budget eventually truncates prose, its recovery normally uses a subagent, and it evaluates bounded probes rather than indefinite resident continuity. The author built the method; some frontier-model commitment results favor the ordinary summary. Its design is pertinent evidence, not a universal winner or a strict zero-model-recovery solution. [Transparency study](https://arxiv.org/html/2610.20116v1)
- ARC uses an append-only observation store with stable recall IDs, permitting exact lookup without reexecuting tools. Its reported recall improvements support investigating addressable retrieval, but benchmark recall is not a guarantee that a resident will notice every missing premise. The archive/retrieval guarantee and model behavior remain distinct. [Addressable recall study](https://arxiv.org/html/2607.25066v1)

The lesson is to measure more than compression ratio or ordinary coding completion. A context that looks plausible but changes a qualification can be worse for this product than an explicit omission the agent can recover.

## Proposed Boxen policy surface

The following are proposals, not implemented settings or settled defaults:

| Mechanism | Extra summarization pass | Original wording in active context | Original archive |
| --- | --- | --- | --- |
| Verbatim projection with observation references | None | Retained spans remain exact; omissions declared | Required and exactly addressable |
| Cold rollover with optional self-authored notes | None required | Whatever original spans/notes the policy carries | Required and exactly addressable |
| Model-written summary plus original tail | Yes, whether same or different model | Summary rephrases; tail remains original | Still required |
| Provider-native compaction | Provider-dependent; opaque transformation | Not certified as verbatim | Boxen-observed archive still required; hidden state separate |

A summary-free mode needs a stricter contract than a template saying “preserve everything important”:

1. History remains original, with speaker/identity, run/thread/branch, ordering, model/runtime provenance, and tool associations. A summary never overwrites the source.
2. Retained utterances are verbatim. Text omitted from the active window is declared as omitted, with stable source ranges/IDs. Extracting a sentence without its qualification is not harmless merely because its characters are original.
3. Direct history tools return original entries or bounded exact excerpts with pagination and neighboring-context access. Search is optional assistance; it is not the only path to exact retrieval. Reading old tool output never reruns the old command or observes today's file instead of yesterday's result.
4. A self-authored note stays labeled as a note by that resident at that time. A helper's account stays labeled as interpretation by that helper. Neither is inserted as if it were an original utterance or proof that an old action succeeded.
5. No summary call or summary fallback is hidden inside a summary-free policy. An optional retrieval assistant can be a separate capability, but raw lookup must remain available. The policy must not silently become summarization when space runs out.
6. Selection preserves coherent tool exchanges and required provider-native state. Images/audio/video need original attachment references and availability, not replacement descriptions falsely claiming equivalent perception.
7. Preparing a new view does not install it. Installation names the source revision/ranges and succeeds atomically at a safe inference boundary. Failure leaves an inspectable source and a usable next step, without requiring an agent's receipt or authored memo before it can continue.
8. Rollover/compaction preserves the canonical Boxen conversation, optional mailbox reply semantics, and outstanding effect uncertainty. It neither creates another resident nor resends incoming messages or completed effects.

At the research checkpoint, the built-in runtime kept recent whole-run groups within a 512 KiB prior-message budget and reports omissions. That was an early summary-free projection and did not provide the token budgeting, deliberate selection, direct history tools, or authored checkpoint control proposed here. “Files remain readable through the shell” was a starting capability, not a finished recall experience. The subsequent [implemented context policies](../context.md) replace that stopgap; the research findings above remain historical evidence.

## The finite-window decision cannot be avoided

Removing old observations usually buys substantial room. Eventually the original user/assistant prose alone can exceed the window. At that point a forever resident needs an explicit policy for which original spans remain active and which become archived/retrievable. Keeping all prose in every request forever is impossible under a fixed token budget.

We should expose that boundary without imposing a summarizer. Candidate policies include a recent verbatim tail with explicit older-span references, agent-selected pinned passages within a quota, and optional self-authored reentry notes. None can guarantee that omitted material remains psychologically salient to a model; faithful retrieval and honest uncertainty are the capabilities we can engineer and test.

A proposed next compaction implementation slice is therefore an exact history read/search tool and versioned, summary-free projection first. This is narrower than choosing default summarizer prompts now. Default mode, prose retention policy, pin budgets, reset authority, and the managed-backend controls need deliberate design before rollout.

## Evaluation before calling a policy successful

Compare policies against the same source conversations, model/provider settings, and effective prompt budgets. Include ordinary workloads and purposeless/resident conversations. Preserve disagreements rather than using a generic judge's fluent-summary preference as the score.

| Probe | What success establishes |
| --- | --- |
| A qualified statement: “possibly, only if X” | Exact wording/qualifier recovery; no invented certainty or broader commitment |
| A statement later corrected/retracted | Original and correction remain ordered and distinguishable |
| A no-reply activation and private narration | Compaction does not convert private text into delivered mail or a reply obligation |
| A completed tool followed by stream failure | Original result stays available; recall does not repeat the effect |
| An absent result after interrupted execution | Unknown stays unknown across projection/reset/restart |
| Two similar episodes with different values | Retrieval returns the correct episode and enough neighboring context |
| A constraint present only in old conversation | Agent can discover it; testing only questions that name the missing fact is insufficient |
| A note by the resident versus a helper summary | Authorship and evidential status remain separate |
| Repeated reductions beyond prose-only capacity | Bounded prompt, explicit omissions, exact paged recall, no silent summary fallback |
| Truncated/empty/failed summary in a summary-enabled mode | No replacement installed from unusable output |
| New input or cancellation during preparation | No lost input, stale checkpoint installation, or duplicate execution |
| Native opaque state and multimodal tool results | Protocol validity and original attachment availability survive the chosen projection |

Measure exact recall, attribution, commitments, uncertainty calibration, continuation success, retrieval burden, latency/cost, and repeated-reduction behavior separately. A resident's own assessment of whether the resulting continuity is acceptable is additional human/agent evidence; a coding solve-rate benchmark cannot settle that preference.

## Sources

Pinned file references below identify the inspected implementations. Current provider documents and research papers are linked alongside their claims. No runtime configuration was changed by this research.

[C1]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/tools/handlers/shell_spec.rs

[C2]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/tools/handlers/view_image.rs

[C3]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/compact_token_budget.rs

[C4]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/session/turn.rs

[C5]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/session/mod.rs

[C6]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/session/tests.rs

[C7]: https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/core/src/config/config_tests.rs

[P1]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/src/core/tools/read.ts

[P2]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/src/core/session-manager.ts

[P3]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/src/core/compaction/compaction.ts

[P4]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/test/suite/regressions/7048-compaction-truncated-summary.test.ts

[P5]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/test/suite/regressions/8328-zero-usage-auto-compaction.test.ts

[P6]: https://github.com/earendil-works/pi/blob/6af92c7950e8e8aaf79248159132f827c7a8b1a6/packages/coding-agent/src/core/extensions/types.ts

[O1]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/core/src/session/compaction.ts

[O2]: https://github.com/anomalyco/opencode/blob/dc4775a64da024bc94f385c85e344d03005e7410/packages/core/test/session-compaction.test.ts

[V1]: https://github.com/vercel/ai/blob/dffe4053c15709577329518044b1ec1d4c6bb0fe/packages/ai/src/generate-text/prune-messages.ts

[A1]: https://github.com/x3haloed/claude-code-upstream/blob/8f12b112ca1edc733e2660c4e4f43e340a83b94e/cli.js

[W1]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/src/session-controller.ts

[W2]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/src/lookout.ts

[W3]: https://github.com/x3haloed/watch/blob/cd823453423891698057a417f6e35ac4c72fe5b1/src/event-log.ts

[B1]: https://github.com/x3haloed/watch-for-business/blob/493455f770979186e25b1fe28032a994257f7867/src/codex/handoff.ts

[B2]: https://github.com/x3haloed/watch-for-business/blob/493455f770979186e25b1fe28032a994257f7867/src/context/reentry-context.ts

[G1]: https://github.com/garrytan/gbrain/blob/0ca2e86acb6a0feaa915a5e6c108bcce4f31f164/docs/guides/brain-vs-memory.md
