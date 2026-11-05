# Inference incidents and continuation

Provider rejection is an execution outcome, not undelivered mail. The original
mail remains presented and the interrupted run retains its accepted input and
observed provider frames. A payment failure does not require a daemon restart,
clearing history, acknowledging uncertainty, or resending the incoming message.
New ordinary messages and scheduled activations remain available independently.

## Coalesced health

`inference_health` projects existing owner-scoped run journals. It returns up to
50 `incidents`, a `total`, and `next_offset`; subsequent requests accept `offset`.
There is no second incident database and opt-in diagnostics are not required.
Rows coalesce by canonical thread, inference purpose and failure category. They
retain counts, first/latest times, latest source run and model, observed HTTP
status, and active/resolved state. No provider body, credential or private message
text is included. Existing retained failures are discoverable after upgrading.

HTTP 402 is classified as payment, without claiming that an exhausted balance is
the only explanation: credit, request token allowance and provider billing can
all affect acceptance. Authorization, rate-limit, rejected request, provider
server failure, unavailable credentials and transport/protocol failures have
separate categories. A later supported completed exchange for the same thread
and purpose resolves active categories. This establishes successful inference,
not success of prior interrupted work. A later failure reopens its category and
increments its count. Compaction success does not resolve workload failure.

The built-in runtime supplies up to eight recent coalesced categories in its
next system context, with source run IDs and explicit consequences. Repeated
failures do not append one agent-facing error message per attempt. Original run
records remain exact and independently retrievable; older prepared inputs retain
the historical context they actually received.

## Explicit continuation

```sh
boxen inference-health HOST:42424 PEER_KEY
boxen inference-resume HOST:42424 PEER_KEY FAILED_RUN_ID --id NEW_RUN_ID
```

The API equivalent is `{"op":"inference_resume","failed_run_id":"…","id":"…"}`.
It starts a new run in the same canonical conversation. A resident receives an
ordinary activation, including newly queued mail; an anonymous thread retains
normal request/response behavior. The prior run must be interrupted, use the
built-in backend, and still be the latest run in that conversation. A source
reference and causal continuation notice become the new input; the previous
prompt is not resubmitted and historical tools or outgoing mail are not
redispatched. The model can reason from retained prior input and results.
Previously unknown effects remain unknown.

A stable new run ID makes exact retries idempotent, including after restart and
later conversation evolution. Conflicting IDs and stale source runs reject.
Acceptance means the new run exists, not that billing is repaired or inference
will complete. Repeated payment failures remain visible in the same incident.
There is no automatic billing probe, retry timer, credit purchase or effect replay.

GUI delivery is deferred. The authenticated paginated incident API exposes durable
source-run references, counts and active/resolved state for future user alerts or
custom clients. Incident discovery is independent of mailbox delivery and optional
diagnostic logging.

`scripts/inference-health-smoke.py` uses paired daemons and a controlled provider
to test repeated 402s, coalescing, restart, owner isolation, next-input notices,
completed tool preservation, stale/exact-ID continuation and normal anonymous
responses. Live billing failures are separate evidence; tests do not spend credit.

Machine-operator queries now aggregate host-local incident namespaces across original owners; resident context and runtime operations retain canonical identity scope. See [network](network.md).
