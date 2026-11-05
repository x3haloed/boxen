# Import source-attributed text history

`boxen import-transcript HOST:PORT PEER_KEY REQUEST_JSON_FILE` seeds an empty
OpenRouter resident's canonical conversation. The authenticated API operation is
`{"op":"transcript_import","spec":REQUEST}`. It does not create a resident,
activate inference, deliver mail, or execute historical tools. Native Codex import
remains a separate operation described in [thread-import.md](thread-import.md).

The request contains `id` (a stable new 64-character lowercase hex import ID),
`resident_id`, `expected_revision`, `source`, and `messages`. `source` has required
`provider`, `title`, and `representation` strings and optional `url` and `model`.
An unknown source model remains null rather than inheriting the destination model.
Each message has a unique source `id`, a `role` of `user` or `assistant`, and `text`.
Source IDs may be explicitly labeled synthetic ordinals when native IDs are
unavailable. The operator determines source roles; no summarization rewrites them.

Bounds are 1–1000 messages, 128 KiB per text, and 384 KiB for the serialized request.
Metadata is bounded nonempty text without control characters. System messages,
native tool calls, attachments and provider session state are not accepted. A
rendered browser transcript can preserve displayed text, but cannot claim original
Markdown, hidden prompts/reasoning, or raw voice/audio. The representation field
records these limits. Tool-looking text remains text.

The operation requires authenticated machine-operator access, the expected resident revision and an empty
canonical conversation. Existing history is never overwritten or appended. A
completed import means the history snapshot was accepted, not that any historical
work succeeded. An exact retry with the same import ID returns the original
snapshot, even after resident edits or subsequent execution. Changed input under
that ID is rejected. Interrupted unpublished staging is not accepted history.

The existing run record stores source metadata, source message IDs, import time,
and a SHA-256 of the typed request. Existing `inference_message` journal entries
retain exact role/text. Context reads expose source attribution; future model
inputs contain those messages plus causal framing identifying imported assistant
words as originating from the declared source. Current identity/memory are not
rewritten. Compaction policies retain their existing guarantees and limits; import
does not promise that all history will fit indefinitely or that a model attends to
it. No extra inbox or optional diagnostic log is an authority for this history.

`scripts/transcript-import-smoke.py BINARY` checks paired transport, exact roles,
source provenance, ownership/conflict/storage failure, idempotency across restart
and resident evolution, CLI import, no historical effects, and the actual next
controlled-provider input. This is fixture evidence, not a live-model recall test.
