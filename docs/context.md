# Built-in context policies

The built-in OpenRouter runtime supports two selectable policies on one journal-backed history/projection path:

- `summary`: attributed model summary plus a bounded recent context. This is the default for anonymous threads.
- `verbatim`: original selected messages, historical observation references, chronological source bundles, visible gaps, and optional authored/reviewed notes. No summarization inference is made. This is the default for residents.

Communication is independent: anonymous threads still return ordinary responses, while residents still have mailboxes and private narration. Managed Codex sessions retain native context management; Boxen rejects an explicit built-in policy in a Codex workload submission. A resident definition can retain a built-in policy setting, but it does not override managed-runtime behavior.

This implements the first usable mechanisms from the [reviewed resident design](compaction.md). It is not a claim that the resident's migration criteria are satisfied or permission to alter her running Watch conversation.

## Selecting and changing a policy

The provisional GUI has a built-in context-policy selector for anonymous submissions, resident creation, and resident definition edits. The daemon stores the setting; browser storage is not required for its execution or recovery. Changing an anonymous policy affects the next accepted request. Omission inherits the latest explicitly configured setting in that thread; a new anonymous thread defaults to summary. Resident activations use the definition's current setting, captured at acceptance.

The authenticated `start` operation accepts an optional `spec.context` object:

```json
{
  "mode": "verbatim",
  "budget_bytes": 196608,
  "pin_quota_bytes": 32768,
  "mask_after": 12
}
```

`resident_create` accepts the same object in its specification; `resident_update` accepts it in its revision-checked patch. Settings replace the object rather than merging arbitrary partial old values; omitted fields within an object take their documented defaults. CLI examples:

```sh
boxen start HOST:PORT PUBLIC_KEY /absolute/cwd MODEL 'A workload' \
  --backend openrouter --context '{"mode":"verbatim"}'
boxen start HOST:PORT PUBLIC_KEY /absolute/cwd MODEL 'A follow-up' \
  --backend openrouter --thread THREAD_ID --context '{"mode":"summary"}'
boxen resident-update HOST:PORT PUBLIC_KEY RESIDENT_ID REVISION \
  --context '{"mode":"verbatim"}'
```

`budget_bytes` is a **serialized context-selection budget**, not an exact token count or a model-window discovery claim. Its default is 196608, with allowed values 16384–524288. Runtime framing, helper text, and notes reserve bounded headroom from selection. Tool schemas, host/identity context, and expanded native media also consume provider input independently; the existing 24 MiB hard request bound remains. A suitable budget depends on the model and its provider route. Automatic model-specific tokenization/window discovery is not implemented.

`pin_quota_bytes` defaults to 32768 and is at most half the configured selection budget. A small budget therefore needs an explicit smaller pin quota. `mask_after` is the count of recent source entries normally exempt from observation masking (default 12, maximum 256). A required latest exchange that would otherwise overflow can have its unpinned tool body masked earlier. If the current request and required exchange still cannot fit, the run reports a legible shortage; it does not discard them and ask the model to repeat the work. Source remains retained for continuation with an appropriate setting.

Policy changes rebuild from original accepted messages. An old helper summary does not become source speech, and switching into verbatim mode drops automatic summary interpretation. It does not claim to undo interpretations already made by a prior model.

## Exact history and authored records

Every built-in thread has `boxen_history`, independent of identity attachment. Its operations also have a first-class authenticated client API:

```json
{
  "op": "context",
  "thread_id": "64-character-thread-id",
  "args": {"action": "read", "offset": 0, "limit": 10}
}
```

The CLI sends that operation without a GUI:

```sh
boxen context HOST:PORT PUBLIC_KEY THREAD_ID '{"action":"status"}'
boxen context HOST:PORT PUBLIC_KEY THREAD_ID '{"action":"read","id":"RUN_ID:EVENT_SEQUENCE","before":2,"limit":5}'
boxen context HOST:PORT PUBLIC_KEY THREAD_ID '{"action":"search","query":"an exact phrase"}'
boxen context HOST:PORT PUBLIC_KEY THREAD_ID '{"action":"coverage","offset":0}'
```

`read` returns accepted `inference_message` records with stable `RUN_ID:EVENT_SEQUENCE` addresses, original timestamps/roles/content, and run/thread/runtime/model/resident provenance. `before` provides up to ten neighboring preceding entries. External API pages contain up to fifty records and approximately 48 KiB. Model-facing history replies use a 2–4 KiB allowance derived from the active selection budget, with smaller exact excerpts and pagination rather than a large result subsequently masked out of the input. A large entry returns an exact UTF-8-bounded excerpt of its serialized message JSON, with `next_byte_offset`; `id`, `limit:1`, and `byte_offset` continue that entry. Literal `search` searches original serialized messages, not a helper's interpretation, and starts large excerpts near a match. Retained native attachment references remain references; reading them does not reobserve today's source file or execute a historical command. Pinning an available media-bearing original makes it eligible for the ordinary native snapshot projection again.

`coverage` partitions the installed boundary's source prefix into `verbatim`, `observation_reference`, and `omitted` entries. It supports `boundary_id` and offset pagination; large associated note/exclusion records use bounded previews and exact `record` lookup. Later live-tail entries are outside that boundary's prefix. Entry counts are not importance or semantic-completeness scores. The source-ID lists plus source watermark in the installed boundary are the coverage authority; paged maps derive from them without another mutable history index.

`pin` takes a stable 64-character hex bundle `id` and `entries` containing 1–64 source IDs. The bundle includes necessary tool-call/result units and appears in original chronological order; a notice distinguishes nonadjacent excerpts. A promise, later qualification, correction, and release can be explicitly linked. There is no automatic semantic-dependency detector. `unpin` releases the bundle preference, without erasing source. This version supports 128 bundle IDs; existing IDs can be updated.

Pin admission is deterministic: the current request/latest exchange take priority, then bundles in lexicographic ID order, then the recent coherent tail. Whole admitted units remain exact. Reservations that exceed quota generate `pin_warnings`; `excluded_bundles` additionally identifies bundles actually absent or masked in the resulting input. An unreserved bundle can still happen to be present in the tail, without acquiring a retention guarantee. Every genuinely excluded declared bundle retains its reason and source pointers. Re-entry shows bounded previews, total counts, quota usage/near-limit information, and the route to the complete boundary record. Pin accounting includes conservative per-entry framing allowance; it is not exact model-token accounting.

`note` records resident/agent- or operator-authored continuity text. `draft` records helper text with declared attribution, original source IDs, and unapproved status. `approve` names the exact note ID/revision, and succeeds only when its referenced source entries are selected unmasked in the latest recorded workload-input exposure. This proves prepared availability, not provider receipt, attention, or comprehension. Approval retains helper authorship. A subsequent edit creates a new revision and does not inherit approval; an unreviewed replacement draft leaves the last approved revision active until adoption; existing-note edits require `expected_revision`. `kind: semantic_signpost` uses the same provenance/review mechanism rather than adding inferred topics to mechanical gap records.

There are at most 32 note/signpost IDs per thread, each with 1–8192 text bytes and bounded provenance. New IDs beyond the cap are rejected; existing IDs remain editable. Included approved notes share one-eighth of the selection budget; excluded approved notes remain available and are counted/reported separately. Status pages expose bounded previews, bundle/note totals, and independent cursors. Preview arrays carry `FIELD_total` counts and `preview: true`; text prefixes carry `text_preview: true`. `record` with an exact context-record ID (and optional note `revision`) returns the immutable journal event, using `serialized_record_excerpt`/`next_byte_offset` pagination beyond 16 KiB. A boundary preview never claims its sampled arrays are the complete map. Optional `parent_note` records immutable ID/revision lineage and derivation depth. Source pointers and review exposure remain distinct. This version exposes that lineage for inspection; automatic re-derivation and inferred obligation/signpost suggestions are not implemented. Helper drafts can be supplied through this API/tool, but automatic helper-draft generation/review UI is not provided.

External mutations are accepted only while the thread is inactive, serialized against new submissions. In-run tool mutations occur at the runtime's tool boundary and affect the following projection. Owner checks prevent cross-peer history access. `compact` requests evaluation of the configured policy at the next safe inference boundary; it never changes policy. Review and authored notes are never prerequisites for continued execution.

## Boundaries, summaries, and failure

Original accepted messages remain in run journals. Installed `context_boundary` records contain their source watermark, settings, source counts, selected/masked IDs, bundle/note admission and shortage records, parent boundary, and any separate summary interpretation. Appending an unchanged live tail does not itself create a cut. `context_exposure` records each prepared workload input's source selection, separately from installation. Interrupted preparation cannot turn partial helper output into accepted source or a completed tool result.

Older large tool bodies are reduced first, with historical observation pointers. Bounded history-tool results are exempt from age masking; under budget pressure their coherent exchanges can still leave the selected tail. The latest recall reply remains unmasked for its first workload input, or execution reports an explicit shortage. Tool exchanges remain coherent; missing results receive explicit unknown-result repairs without dispatch. Bounded re-entry framing includes installation/source/input timestamps, recent mechanical gaps, older omission counts, attributed notes, summary provenance/limitations, and shortages. The first input after a cut carries an optional audit affordance. There is no required receipt or acknowledgment.

In summary mode, omitted source projections are sent to a tool-less helper request using the workload's model/route. Inputs are bounded to half the selection budget; older large observations can be represented by explicit historical references. A previous summary is labeled as interpretation, separately from newly supplied source. Each accepted summary records its actual source IDs, masked-source caveat, model, derivation depth, parent boundary, and timestamp. These links do not claim to summarize every omitted entry. The original archive is independently readable.

A helper result is accepted only after a supported completed projection with normal `stop`, nonblank text of at most 8192 bytes, and no tool calls. Truncated, empty, malformed, disconnected, or call-bearing output is rejected. Frames stay journaled with `purpose: compaction`; streamed text uses `context_delta`, not workload answer events. The helper never executes tools or sends mail.

On helper failure, the policy remains `summary`, preserves any previously accepted interpretation with its old provenance, and records an explicit degraded-continuation reason alongside bounded raw context. It does not silently select a different mode or accept partial prose. The attempted source watermark prevents repeatedly retrying the same failed prefix at every tool step; later newly omitted material can cause a new attempt. Cancellation terminates the helper child, records cancellation, and does not install its candidate. Steering arriving during preparation is applied before the next workload inference. Exact history, diagnostics-off execution, mailbox delivery, and viewer replay remain independent of summaries.

There is no source deletion by compaction. History/media retention and replication quotas are still separate migration work; no infinite storage promise follows from source retention in this implementation. Native signatures/state are retained for selected messages, but broad provider-specific validity after arbitrary pruning remains an integration risk to test per route. A provider rejection stays legible rather than triggering tool replay or hidden runtime switching.

## Verification

`scripts/compaction-smoke.py BINARY` uses two real paired daemons and a controlled provider for repeated reductions, exact history, source bundles, coverage, draft isolation/review, policy switching, summary rejection/cancellation, observation masking, restart, and non-reexecution. Rust tests additionally exercise chronological qualification bundles, latest-effect preservation, pin overflow, exact Unicode pagination, and unchanged live-tail behavior. The Linux rotation includes this suite.

`scripts/compaction-live.py BINARY [MODEL]` defaults to `openai/gpt-4.1-mini` and uses the local OpenRouter key, disposable paired identities, randomized invented commitments and unrelated bulk. It performs nine workloads per mode at equal 64 KiB selection budgets and reports exact retention separately from whether a non-leading planning response quotes the original qualification and whether the original commitment/qualification are present unmasked, either as directly selected originals or as original lookup results. These two evidence paths are reported separately; a verbatim view does not require redundant rereading of originals already present. It never accesses the resident's Watch state. Real-model outcomes are evidence for the tested histories/model, not general continuity certification.

### Evaluation correction during implementation

The first live check accepted a matching random tag/name/deadline phrase as evidence of recall. Raw responses contradicted that conclusion: lookups could be replaced with omission notices, and a model could repeat a tag without recovering the qualification. Those keyword-only pass labels are invalid evidence of usable recall; exact archive-retention results remain independently valid.

The corrected path bounds replies at their producer, preserves the latest recall result through first consumption, and exempts bounded recall results from age masking. Controlled tests inspect the actual next provider input. The live check additionally requires the original qualification quotation and journal evidence of original-source availability in the final workload, either directly selected or through unmasked original lookups. Forced-reader controlled cases independently exercise lookup delivery, so recognizing directly present originals does not excuse a broken reader. Randomized new histories are used for subsequent checks; earlier transcripts are retained locally rather than rewritten into successes. A small-window summary stress run also reached the existing 16-exchange limit during repeated lookup, and a stricter DeepSeek verification task hit the existing 2048-output-token limit; suitable model budgets and bounded retrieval burden remain part of evaluation, not a promise of unlimited verification within one activation.

### Verified implementation evidence — October 31, 2026

The identical 9,294,380-byte APE (`5cd350b24479fbd69e0a039ae7174e0745dd4b95d0e43bed95a35294384012a2`) passed the eleven controlled suites on macOS ARM64 and Debian 13 x86-64. The native macOS build passed its ten applicable suites; 21 Rust tests and strict Clippy passed. Actual APE/native timeout, wakeup, and idle-CPU probes passed. Linux direct LAN pairing, early workload streaming, and 46-event gap-free replay passed without SSH forwarding; the temporary Linux bundle was removed.

The actual APE also completed nine live OpenRouter workloads per policy using GPT-4.1 mini at equal 64 KiB selection budgets. Each mode installed four views, retained exact source, quoted the original qualification, and delivered original lookups unmasked. Verbatim made zero helper-summary calls; summary made three. The verbatim case additionally had both originals directly selected. Reported provider costs were $0.0736312 and $0.0862028 respectively. These are observations for invented histories and that model/route, not a model-independent semantic guarantee.

Other live cases remain limitations, not successes relabeled after the fact: Nano changed a literal identifier in a summary, and DeepSeek's more involved verification ran into the prototype output/exchange caps. Helper guidance now explicitly preserves literal spelling and turn-local instruction scope, but those words do not certify factual fidelity. The default runtime model is unchanged; the live verification script's documented default uses Mini for this stronger probe.

A real Chrome session retained an anonymous policy selection, created a resident with a selected policy, and changed its setting through the revision-checked API without browser errors. Both development Mac daemons were updated to the verified APE while idle, retained their journals/identities, and advertised both policies afterward. The resident's Watch service, identity, memory, and live conversation were not modified by implementation or tests. No migration was performed.
