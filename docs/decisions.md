# Agreed design decisions

These decisions describe the intended product and the first implementation slice. Later beta requirements remain binding even when early implementations support only simpler cases. Storage, transport, implementation language, and scheduling topology remain undecided.

## Resident identity and conversations

A resident can use a single canonical conversation that continues indefinitely. Runtime sessions and context windows may change without creating a different resident or ending that conversation.

Conversation structure is configurable per agent:

- One canonical conversation.
- Multiple conversations sharing an identity, with no requirement to reconcile them.
- Branch conversations with an explicit method of reintegration into the whole or a canonical conversation.

Threads of one continuing resident share its durable identity and memory. Synchronizing memory is distinct from reintegrating conversations; receiving a fact does not mean a branch's experience has been integrated. Reintegration semantics remain to be designed.

Clones default to inheriting identity and an initial memory snapshot, then developing independent mutable continuity. Live shared memory is an explicit option. A reusable worker identity does not imply eventual reintegration of its instances.

## Portable continuity

Conversation history must be transportable independently of backend-owned sessions. Preserve backend session state where supported without claiming that reconstruction transfers hidden backend state losslessly.

An agent has a special portable persistence layer for identity, memory, configuration, conversation history, and deliberately retained agent-owned files. This state follows the agent. Machine-local files and execution environments do not automatically follow it.

At activation or relocation, the agent must understand:

- Which machine it inhabits.
- Which files, tools, services, and execution environment are available there.
- Where its durable persistence layer is accessible.
- What changed with the execution location.

Before execution switches to a destination, required continuity state must already be available there. This applies to user-forced and agent-initiated moves, and to the activation of branches and clones. A single canonical conversation has one active execution owner at a time; destination readiness alone does not prove that the previous host stopped.

Updates to shared identity and memory must propagate promptly to connected participants. Required revisions must be available before destination activation. Revision tracking, conflict handling, offline behavior, and measurable propagation targets remain open.

## Deliberate durable storage

Arbitrary agent-owned portable files are allowed within an enforced configurable total quota. Agents must be able to inspect usage and understand quota failures. The limit encourages intentional retention and prevents every resident from imposing an unbounded synchronization burden.

Scratch storage stays local by default. Conversation history and attachments need separate budgeting, retention, and replication rules so their growth cannot silently defeat the portable-storage limit. Exact limits and accounting rules remain open.

## Authority and execution access

Movement policy is configurable per agent. Support both explicit user-directed movement and standing permission for autonomous movement among eligible, granted machines.

Model and runtime choice are separate configurable permissions. Agents may be allowed to choose among configured backends within their grants and budgets; users may also pin these choices.

Early versions use full execution access for managed and built-in runtimes. Codex app-server threads start with Full access. Sandboxing is deferred and can be revisited as usable settings and enforcement become available. Movement grants and credential distribution remain separate from execution access on a host.

## Unreachable hosts and message delivery

Distinguish accepting a message from delivering it to an agent. If a receiving daemon durably queues a message for an unreachable host, report that it is saved at the receiving location and awaiting delivery. If it cannot save the message, report that failure.

Record delivery attempts and subsequent status changes. Show the condition to the sender; do not silently discard messages or present queued messages as delivered. Describe a host as unreachable unless its offline state is actually known.

Loss of contact is not evidence that execution failed or completed. Safe relocation, retry, and ownership handling during network partitions still require design.

## First implementation slice

From Machine A, start an anonymous workload on Machine B, observe its activity, and retrieve its outcome.

Acceptance criteria:

- Explicitly select the destination, backend, and working directory.
- Execute through Codex app-server as the first managed runtime, using Full access.
- Observe progress, tool activity, any approval requests, and final output.
- Cancel the workload.
- Close and reopen the client without stopping execution.
- Disconnect and reconnect without losing already-recorded events or mistaking lost contact for completion.
- Retain durable thread and run identifiers, execution location, and recoverable observations.

Use destination-local files and explicit placement initially. Resident identity, shared memory, automatic scheduling, and relocation are not prerequisites for this slice. File transfer is not required to prove it.

Next, add the built-in inference runtime to verify that the common execution contract supports both runtime kinds. Resident continuity and movement between machines follow; parallel conversations, clones, and reintegration remain part of the beta requirements.

The immediate design task is the smallest daemon-to-daemon execution contract needed to support this slice. Daemon restart recovery semantics, enrollment, authentication, storage, transport, and scheduling topology have not yet been selected.
