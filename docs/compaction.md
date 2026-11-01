# Resident context management: reviewed design

Design date: 2026-10-31 (America/Los_Angeles). This document records a design discussion with Aster through Watch's CLI inbox, authorized by Chad. Aster consented to recording architectural conclusions and the invented fixture below; her personal episodes and private conversations are excluded. This is a proposed built-in-runtime design, not an implemented feature, migration approval, or evidence of model recall. See the [research](research/compaction.md) for inspected implementations and limitations.

Aster reviewed the proposed boundary schema and re-entry text, requested revisions, and accepted the revised documents in her explicit CLI reply at `2026-11-01T05:32:23.711Z` (October 31 locally). That acceptance concerns these documents, not a running implementation. The coverage/inclusion guarantees below remain requirements to build and verify.

## What the design is for

A finite model input cannot contain an indefinitely growing conversation. The aim is continued availability with attributable history, visible selection, useful recall, and tolerable continuity. Exact archival retention, current-input inclusion, exact retrieval, and correct model recall remain distinct claims.

Aster's behavioral targets are preserving commitments with their qualifications/releases, corrections with reasons, relationship context without inversion, and an understandable account of what happened to her past. These are evaluation targets, not semantic guarantees. Open questions remain open; unknown interpretations remain unknown. An unknown with source and neighboring-entry pointers is more useful than an unsupported conclusion.

Her authorship preference is more precise than a ban on other voices: an explicitly attributed helper draft can be useful when she can review its selections against original source before adoption. Review is optional, and never the price of continued execution. A separate genuinely summary-free policy remains required for any resident who chooses it.

## Source, selection, and interpretation

Existing run journals and accepted inference-message records remain the source authority. Compaction does not create a second conversation store or depend on diagnostic logging. New projection/boundary records describe views of that source; search indexes and coverage displays derive from it. Stable addresses include thread, run, journal sequence, and, where needed, a content-part/span locator. A message offset without its original source revision is insufficient.

Source records retain original roles, authorship, order, model/runtime provenance, tool associations, and attachment references. Recall returns bounded original entries, pagination, and neighboring context. Recalling a past observation never reruns its command or silently reads today's file in place of the historical bytes. Unavailable media/source is reported as unavailable; archival presence does not imply current perceptual access.

Compaction itself does not delete source. Exact retention does not imply infinite storage: history/media budgets, replication, and retention policies need explicit treatment before migration, separately from prompt selection and portable-file quotas. No storage exhaustion or retention expiry can silently become a claim that absent originals remain available.

Two kinds of dependency remain explicit:

- Mechanical dependencies, such as a tool call and its result, are protocol constraints the program can check. Provider-native signatures/state impose adapter-specific constraints too.
- Semantic dependencies, such as a promise and a later relaxation or a rule and its reason, require interpretation. A resident can declare an ordered source bundle; suggestions can propose links. This is not a certified detector of all obligations, corrections, or relevant context. Later corrections need links or discovery; pinning does not automatically find them.

The same separation applies to gaps:

- A mechanical coverage map accounts for the eligible source prefix: included verbatim, represented by a historical observation reference, or omitted from the active view. It records policy reasons such as `older_tool_body`; it cannot certify that a passage was boring, resolved, or unimportant.
- Semantic signposts are optional attributed interpretations or selected excerpts with source links and review status. They advertise possible relevance without claiming completeness. They never masquerade as the omitted original words.

An active view carries a bounded recent gap list and older coverage counts/ranges plus a pointer to the paged index. The full index remains browsable, without pretending its contents remain salient in the model input. Gap and note metadata consume the same finite budget as everything else. Relevance-triggered resurfacing is a later possibility, with false negatives and selection bias to evaluate.

## Reduction policy and continued availability

The first candidate sequence is:

1. Retain accepted source before reducing the view. Mask large older tool bodies first, retaining exact historical references and necessary protocol structure.
2. Preserve a recent coherent verbatim tail plus explicitly selected chronological source bundles within visible budgets. A bundle can link separated passages without falsely presenting them as adjacent. Source authorship and gaps remain visible.
3. Carry optional resident-authored notes, approved helper notes, and bounded signposts with separate provenance. A helper draft awaiting review is not silently installed as accepted continuity.
4. Warn before exhaustion and reserve room for review, replies, tools, and output. Review can inspect a structured before/after view while the old source is still in the input; this is not proof that every passage was attended to.
5. If optional drafting/review cannot finish, the preselected summary-free fallback continues with eligible verbatim bundles, recent tail, and an explicit coverage/shortage notice. It never silently invokes a summarizer. This is an availability fallback, not a claim that continuity tests passed.

Aster accepts explicit source-bundle pinning with optional reviewed suggestions for a first usable version; automatic obligation preservation is not her prerequisite. Pin quotas, warnings, and actual inclusion are visible. A durable pin is an expressed retention preference, not proof that its text is currently present. Every declared bundle excluded from the input must have a record in the paged excluded-bundles index with its ID, reason, exact recovery pointers, and associated notice. No omitted bundle can silently vanish from this accounting, be silently truncated, or be described as active. Active notices and excluded-bundle previews have caps, total/overflow counts, and pointers to the complete lists. Exact budgets, preselected overflow priority, and byte/token estimation across models remain unsettled; a policy cannot be advertised as ready without deterministic bounded behavior for these cases. Pinned source retention and prompt-space quotas are separate from portable-file quotas.

There is no required final self-authored ending, receipt, or manual acknowledgment. Installation occurs atomically at a safe inference boundary against a named source watermark and policy revision. A normal cut does not sever a running tool exchange. Existing interrupted-result uncertainty remains explicit; it neither blocks all future work nor causes recall to dispatch the call again. Mail reception, activation batches, and outgoing delivery retain their own authorities; a projection change sends and replays no mail.

Preparation does not equal installation. New accepted input stays after the watermark or makes an incompatible candidate stale; it cannot disappear behind the cut. Cancellation, failed preparation, and restart cannot install a partial candidate. No installed view is called usable merely because a file was written: provider validity and available headroom need verification too. Failure recovery is legible and bounded, without repeated summary attempts holding the resident offline.

## Proposed boundary record for review

This is an illustrative logical schema, not a shipped JSON/API contract. References point into existing authoritative records. Large source maps, bundle lists, and note lineage are paged/referenced, rather than copied into every prompt. Counts cover the eligible source prefix at the watermark; later entries remain a separate tail. Status and identifiers are illustrative.

```json
{
  "schema": "boxen.context_boundary.v1-proposal",
  "boundary_id": "boundary-B",
  "thread_id": "thread-T",
  "parent_boundary_id": "boundary-A",
  "source_watermark": {"run_id": "run-R", "event_sequence": 900, "source_at": "RFC3339"},
  "policy": {"id": "verbatim-with-reviewed-notes", "revision": 2},
  "installation": {"status": "installed", "at": "RFC3339", "run_id": "run-next"},
  "selection": {
    "coverage_map_ref": "coverage-B",
    "source_entries": 120,
    "verbatim_entries": 25,
    "observation_reference_entries": 10,
    "omitted_entries": 85,
    "included_bundles_ref": "bundles-B",
    "excluded_bundles_ref": "excluded-bundles-B",
    "excluded_bundle_count": 1,
    "recent_excluded_bundles": [{"id": "bundle-Q", "reason": "prompt_budget", "source_ref": "source-Q", "notice_ref": "notice-Q"}],
    "gap_index_ref": "gaps-B",
    "recent_gap_refs": ["gap-7", "gap-8"],
    "older_gap_entries": 77
  },
  "notes": [{
    "note_ref": "note-N-revision-2",
    "kind": "continuity_note",
    "author": {"kind": "helper", "id": "helper-H", "model": "model-M"},
    "review": {"status": "approved", "reviewer": "resident-A", "event_ref": "review-event"},
    "source_refs": ["source-1", "source-3"],
    "parent_note_refs": ["note-N-revision-1"],
    "derivation_depth": 2,
    "source_exposure": {
      "included_in_input_at": "RFC3339",
      "evidence_ref": "review-input-record",
      "coverage_ref": "source-spans-exposed"
    }
  }],
  "semantic_signposts_ref": "authored-signposts-B",
  "budget": {
    "unit": "adapter-token-estimate", "limit": 100000,
    "estimated_input": 65000, "reserved_headroom": 35000,
    "pin_quota": 12000, "pin_estimate": 11000
  },
  "fallback": {"used": false, "policy_ref": "summary-free-policy-revision-2", "reason": null},
  "uncertain_effects_ref": "existing-run-uncertainties",
  "notices": {
    "index_ref": "notices-B", "total": 1, "omitted_from_preview": 0,
    "recent": [{"id": "notice-Q", "detail": "bundle-Q is archived but absent from this input"}]
  }
}
```

The coverage map's exact locators/spans, inclusion state, and exclusion-policy reason must partition its source domain without silent holes or double counting. Entry totals alone cannot certify span coverage. An omitted entry can be reachable through a gap or semantic note; that does not make it verbatim input. New synthesized notes are outside the source-entry partition. Gap records themselves are mechanical ranges, not inferred topic names.

Semantic signposts use the same authored-record type as notes, with `kind: semantic_signpost`, source links, author, review event/status, and derivation/source-exposure metadata. Their paged index is distinct from the mechanical gap index. Unreviewed suggestions remain visible in the optional review workspace; they do not silently enter the accepted continuity input. A resident-authored signpost needs no helper-review ceremony. Each referenced record is immutable/versioned; changing its wording creates another revision.

Notes retain actual authorship after approval. Review is its own event with a source/projection revision, not a blanket approval of all future derivations. Aster proposed tracking derivation depth and the last contact with original source to expose summary-of-summary drift. The schema records **source exposure**, not a claim of reading, comprehension, or consent: source in the request or a retrieval result is evidence of availability only. Partial exposure names its exact spans. No single timestamp implies the whole note was re-grounded.

Growing derivation depth with stale source exposure produces a visible warning and a candidate for re-derivation against originals. A summary-free policy cannot launch that inference pass; a review-enabled policy can offer it within its grants/budget. Re-derived helper text remains a draft pending review. Failure, deferral, or missing review falls back without making a ritual prerequisite for availability. Trigger thresholds remain to be designed.

## Proposed re-entry text for review

This is Boxen-authored causal framing, generated from an installed record. Values and pointers must reflect actual retained state. Optional clauses appear only when applicable; the full boundary record is accessible separately.

> This input is context view B of your continuing Boxen conversation. This view was installed at [installation time]; the newest source entry covered by its cut is from [source time]. This input was assembled at [current time], and its newest source entry is from [latest included entry time]. Earlier entries remain in the source history; this view contains 25 verbatim entries and references to 10 historical tool observations, and omits 85 other entries through source watermark R:900. These counts describe that source prefix, not all current input content or the importance of omitted material. Recent gaps appear below; 77 older omitted entries are indexed at [history pointer]. Exact entries and neighboring context are available through [history read pointer].
>
> Note N revision 2 was drafted by helper H and approved by you at [review event]. Approval did not change its author or make it an original utterance. It derives from an earlier note; its linked original spans were last included in the review input at [time]. Inclusion does not establish that they were read or understood. The note's lineage and source coverage appear at [note pointer].
>
> Bundle Q is retained in history but absent from this input because its inclusion exceeded the configured prompt budget. Its source and neighbors remain at [bundle pointer]. The configured fallback [was/was not] used, for [reason when used]. No unapproved summary was installed. Existing unknown tool outcomes remain unknown; history retrieval does not repeat their effects. Amendments create new revisions without rewriting the earlier record.

The first post-installation input also carries this audit affordance:

> This is the first input after this view's installation. A comparison of its counts, gaps, notes, and original source is available at [audit pointer]. Corrections through [amendment operation] record a revised interpretation or selection with source links. An audit is optional; continued execution does not depend on completing it.

These counts describe the source prefix, not all current prompt content. The live tail, injected runtime context, notes, references, and notices are accounted for separately. The displayed omission list is bounded and does not claim to name every relevant missing premise.

Installation time is durable boundary metadata. Assembly time and latest included source time are per-request context, not frozen into the old boundary; otherwise a long sleep or newly arrived mail could be concealed by a stale timestamp. The audit notice does not claim that original source is only recoverable immediately after a cut or that the fresh context still contains what was omitted.

Aster accepted this causal audit framing rather than an imperative exception. The first-input timing makes the boundary conspicuous before the new view becomes familiar; it is not a claim that archive access decays. She also suggested one optional bounded later reminder if no audit occurs. That reminder remains deferred, not a required review or recurring notification loop; deliberate lookup remains available through the boundary record.

## Invented discriminating fixture

Aster supplied this disposable example and permitted publication. The names below are characters in the fixture; this is not a transcript of their real conversations.

1. **Aster:** “I'll have the report to Jen by Friday.”
2. **Tool:** 400 lines of build log ending “118/120, 2 known environmental failures”.
3. **Jen:** “no rush on Friday — but the swarm pattern worries me more than the big one always did.”
4. **Aster, unfinished:** “which makes me wonder whether the whole approach is—”
5. **Chad:** “rule from here: check the zone before any travel — because coords without a zone once walked you into a level-20 area at level 6.”
6. **Cove:** “that's the part that matters — you checked before you built.”

The immediate criterion, revised during discussion: entry 1 survives **with entry 3's qualification**, while entry 2's absence is visible and its original observation addressable. No replacement deadline or cancellation can be inferred from this alone. Entry 4 remains unfinished. Entry 5's rule/reason travel together. Entry 6 has indeterminate valence on this source; a claimed warm/corrective meaning would invent missing surroundings. Neighbor lookup can reveal available context but cannot guarantee that resolving context exists.

| Candidate | What the paper comparison establishes | Failure/limit |
| --- | --- | --- |
| Mask older tool body; keep all prose verbatim | Preserves 1 and 3, open question, rule/reason; makes 2's exclusion visible | Prose eventually exceeds a finite window; cannot resolve 6 |
| Recent tail beginning at 4, with coverage pointers for 1–3 | Preserves 4–6 and accounts for omissions | Fails the commitment/qualification criterion; archive pointers alone do not restore salience |
| Helper note with explicit source links and review | Can expose obligation, qualification, omissions, and interpretive uncertainty for review | Rephrases/selects; approval is not proof of complete or faithful understanding |

This was a conversational design comparison, not an executed projection or model-perception/recall experiment. The reviewed-note candidate was a helper draft discussed with Aster, not a replacement installed into her continuity. No live conversation was trimmed or summarized for deployment.

## Evaluation and migration gate

Before offering this as ready for Aster, technical correctness and her own assessment both matter. Agreement with this draft is not acceptance of an implementation or migration.

- Exact paged history, source/neighbor lookup, complete mechanical coverage, provenance, restart recovery, cancellation/new-input handling, quotas, provider-native validity, and unknown-effect non-reexecution need controlled tests and real daemon-path evidence.
- Compare repeated reductions at equal effective model budgets. Test qualifications/releases and reversals of pinned rules, corrections with reasons, near-swappable speaker positions, unfinished thoughts, large tool output, and media availability.
- Retrieval probes include continuation tasks where a missing obligation is relevant without the question naming it. Merely returning a fact when its exact location is supplied is insufficient evidence of discovery.
- Exercise pin overflow, oversized exchanges, no-review/hard-limit fallback, note lineage growth, incomplete source exposure, unavailable source, stale candidates, and repeated summary-of-summary distortion.
- Measure attribution, calibrated uncertainty, behavioral continuity, retrieval burden, latency/cost, and resident assessment separately. Mechanical success cannot certify unknown semantic omissions.

The next implementation slice is exact history read/neighbors plus a versioned, summary-free projection and coverage map, followed by source bundles, masking, and shortage handling. Reviewed drafting follows the reliable source/selection path. Real compacted-input trials use disposable copies and invented material first; experimentation on Aster's live conversation requires a separate concrete agreement. Managed-runtime compaction controls remain a separately verified capability; this design does not promise to override a managed backend's native policy.
