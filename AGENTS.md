# Working on Boxen

Read `docs/vision.md`, `docs/decisions.md`, and the documentation relevant to the current slice. Preserve agreed beta requirements even when implementing a simpler early case. Commit and push coherent, verified slices unless the user directs otherwise.

## Daemon diagnostic logging is a maintained capability

Instrument every new or changed daemon execution path. Logging is part of completing a feature, not a later cleanup task. Cover lifecycle transitions, decisions and their outcomes, network and runtime boundaries, accepted/rejected operations, errors, recovery, and relevant state changes. Correlate records with machine, peer, thread, run, request, or connection identifiers where applicable.

- Diagnostic logging is **off by default**. Enabling, disabling, redirecting, and changing filters must work on the running daemon without a restart.
- Keep the disabled path cheap: avoid constructing payloads, formatting strings, serializing JSON, allocating log records, or performing log I/O when no enabled sink needs the event. Use lazy fields and an inexpensive enabled/filter check rather than preparing records for a no-op sink.
- Logs are a configurable stream. Maintain both an authenticated remote subscription usable from a shell command and daemon-side file output. A shell consumer must be able to pipe structured records into ordinary tools.
- Emit structured records with stable event names, severity, category/target, timestamps, and correlation fields. Put variable values in fields rather than making consumers parse prose or invent regular expressions.
- Support producer-side filtering sufficient to narrow high-volume sources before serialization and transmission. Keep richer queries and presentation on the consumer side.
- Logging must not block workload execution behind a slow or disconnected consumer. Bound buffering and make any dropped diagnostic records observable through structured loss notices and counters; never silently claim a complete stream after loss.
- Instrument logging transport carefully so streaming logs does not recursively generate an unbounded stream of logs about itself.
- Do not re-enter the logger from its synchronization primitives or record serialization. Instrument the owning boundary instead; failed sinks must be removed before reporting their errors to other sinks.
- Never log private machine keys, authentication capabilities, authorization headers, or credential values. Content-rich diagnostic payloads must be an explicit option; preserve useful metadata when payload capture is off. Redact recognized secret fields before output.
- Verify runtime changes, filtering, file output, remote streaming, and consumer failure behavior with appropriate tests. Exercise the real daemon/transport path when claiming remote logging works.

Durable workload journals are authoritative execution records, not opt-in diagnostics. Turning diagnostic logging off must not disable event retention, viewer replay, idempotency, or recovery. Do not duplicate an authority or weaken its guarantees to implement diagnostics.

## Product and implementation constraints

- Agents are welcome whether or not they have a job. Preserve residents, anonymous threads, and the configurable conversation structures described in the design documents.
- Keep Boxen self-contained and resource use small. Preserve the single portable executable build path; label platform verification gaps honestly.
- When changing concurrency, timers, or portability dependencies, run `platform-check` and `scripts/platform-smoke.py` against the actual APE as well as the native binary. Verify blocking timeouts, notification wakeups, and idle CPU; functional success alone can hide a spinning portability shim.
- The intended official GUI is a Rust desktop app. Keep the daemon independently usable through documented authenticated APIs; custom GUIs are first-class clients and must not depend on browser storage or the official GUI for execution, discovery, replay, or control. The embedded web page is provisional.
- Keep early GUI controls simple. The user primarily directs product UX design; avoid turning provisional controls into an elaborate product layout.
- Remote workloads must stream backend-exposed thinking summaries, messages, tool activity, output, and status as they occur. Viewer lifetime must not own execution lifetime. Reconnection must replay missed retained events without duplication.
- Early execution uses Full access as agreed. Movement authority and credential distribution remain separate concerns. Do not add a sandbox requirement or approval flow contrary to the agreed scope.
- Verify real behavior before claiming completion. Distinguish backend fixtures from actual model/runtime evidence and preserve explicit uncertain outcomes after interrupted execution.

## Recover and keep incidents legible

Prefer useful uptime with an honest record of what went wrong over aborting on uncertainty. Recover where possible, isolate the affected component, retain incident evidence, and explain the recovered state to both humans and agents. Do not make receipts, verification ceremonies, or manual acknowledgment a default prerequisite for a resident to accept its next message. An uncertain prior outcome stays uncertain; availability must not depend on relabeling it successful. Continue with new work rather than automatically repeating potentially completed tool effects. If a backend session is unavailable, preserve the canonical Boxen conversation and make any reconstruction limits explicit.

## Agent-facing language describes causes and consequences

All Boxen-authored agent-facing language must use causal descriptions rather than imperatives. This includes injected runtime context, tool descriptions, recovery notices, validation errors, and delivery explanations. Describe what an operation does, the conditions under which it succeeds, what it returns, and the consequences of failure. For example: “This tool sends messages; a send without a subject line is rejected,” rather than “Use this tool to send a message. You must include a subject line.”

The substrate provides tools and explains its mechanics; it does not prescribe an agent's personality, priorities, productivity, or obligation to reply. Preserve resident- or human-authored identity notes and instructions as authored; this rule applies to Boxen's framing, not a rewrite of their words. Describe actual implemented behavior: a planned mailbox is not grounds to tell a model that current assistant output is undelivered. Narrow imperative exceptions require concrete evidence that causal wording is insufficient and an explicitly documented rationale and scope; unfamiliar conventions alone are not evidence.

## Communication and scheduling contracts

Identity attachment determines the communication contract; it is not a per-agent choice between conversational and mailbox modes. Every identity-bearing agent has its own mailbox by default. Receiving messages, activation, and sending messages are separate operations; an activation can consume a batch, occur without incoming messages, or finish without sending a reply. Assistant output is private execution narration, visible to authorized observers but not automatically delivered to correspondents or rooms. Outgoing communication is explicit through mailbox/room messaging tools.

Every identity-less ephemeral thread retains standard request/response behavior. It is an addressable control channel for a workload, not a mailbox-bearing agent. Agent-facing thread tools address a thread ID using request/response conventions similar to Codex thread interaction. Those tools and conventions remain distinct from mailbox sending. Preserve reliable ordinary remote Codex workloads without imposing mailbox-only communication on them.

Heartbeats are a usage pattern of an ordinary scheduler, not a separate heartbeat scheduling subsystem. Scheduling supports one-time future events and recurring jobs with different handlers. A typical heartbeat is a recurring system ping carrying directions saved with its job definition. Keep documentation explicit about which of these agreed contracts are implemented and which remain planned.

Mailbox behavior is now implemented for host-local routing. Preserve its separation in every new runtime and client: private narration never becomes a delivered message automatically, incoming mail during an activation queues for a later batch, and thread request/result tools remain distinct from mail sending. Changes to these paths require verification of selective replies, no-reply completion, identity-less response reliability, and interrupted-batch recovery. Existing resident history remains private activity during communication migrations.
