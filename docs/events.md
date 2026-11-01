# Observations, subscriptions, and event delivery

Agreed design, November 1, 2026. This document describes planned behavior. The
existing scheduler, mailbox, runtime, media, and execution journals are
implemented; observation sources, subscriptions, and external adapters described
here are not yet implemented. Protocol syntax, numeric defaults, and the open
questions below remain to be designed.

## Purpose and boundaries

An agent can subscribe to a stream of observations, receive bounded batches at a
chosen interval, and independently choose whether a ready batch requests an
activation. This covers passive interests, such as Aster's USGS earthquake feed,
and ongoing feedback for games or other interactive environments.

A subscription can remain unattended without making the agent responsible for
keeping Boxen healthy. Agents and humans choose interests and participation;
software contains runaway retry, buffering, storage, and activation costs.
Residents remain welcome without tasks or outgoing replies.

Collection, buffering, presentation, and activation are separate operations.
Source collection continues independently of model execution. Sampling a source
and presenting its observations may have different cadences. A new stream must
not require Boxen to start an inference merely to collect input.

Identity-bearing agents retain mailbox communication and private narration.
Observations carry source provenance and are distinct from correspondence.
Identity-less threads retain conventional request/response behavior; supporting
observations during a workload does not turn the thread into a mailbox-bearing
resident. Data from a source does not acquire the authority of Boxen instructions
or the human's directions.

## Sources and subscriptions

A source produces observations. A subscription selects observations and defines:

- Batch cadence and size limits.
- Retention and coalescing behavior.
- Whether a nonempty ready batch requests activation.
- Its lifetime and target.

Multiple subscriptions can consume a source through independent positions; one
consumer's presentation cannot drain another's pending input. Exact storage and
sharing mechanisms remain open. Retention, pending bytes, attachments, and
producer input rates need enforceable bounds rather than an unbounded queue.

The ordinary scheduler owns timed delivery and activation requests. No separate
heartbeat or stream-only scheduling service is introduced. Several subscriptions
becoming ready together can request one activation. A busy target retains bounded
pending input rather than accumulating an activation for every missed tick.
Configurable per-subscription and aggregate target limits bound automatic wake
requests. Reaching a limit leaves a visible subscription condition and bounded
pending input; it does not disable the recipient's unrelated mail or work.
An empty batch does not itself require inference; an independently configured
heartbeat remains a legitimate empty-inbox activation.

With waking disabled, collection continues and pending observations become
available at the next suitable activation or explicit read, subject to retention
limits. Retained input, prompt inclusion, and agent understanding remain separate
claims. Exact batch sealing and presentation boundaries remain open.

## Ordered events and current state

Two explicit buffering contracts are needed:

| Form | Examples | Pending input |
| --- | --- | --- |
| Ordered events | Earthquakes, messages from an environment, completed actions | Retain order within bounds and expose gaps. |
| Current state | Camera image, position, visible world | Replace superseded pending snapshots and retain freshness information. |

A source may supply both. A new screenshot does not erase an action's failed
result. Old screenshots do not establish the current state of an environment.
Domain-specific interpretation or meaningful-change filtering belongs to the
adapter, with its transformations declared rather than silently presented as raw
observations.

Batches carry source identity, sequence/revision information, observation time,
and explicit loss/coalescing metadata. Source time and receipt time have distinct
meanings. A restart or reconnect cannot silently reinterpret old data as fresh.
Finite retention can lose unpresented input; bounded loss records describe what
is known about the missing interval or sequence range. Unknown counts remain
unknown.

Accepted model inputs retain exact provenance and required attachment snapshots
through existing continuity/journal authorities. Pending retention is distinct
from already accepted conversation history. Presentation is not proof of reading,
understanding, or a successful action. This state remains inspectable with
diagnostic logging disabled.

## Source health and failure containment

Connector health has a bounded, inspectable representation independent of the
observation payload stream. It records current condition, first/latest failure,
retry count, last successful observation, and next retry or suspension state.

A meaningful transition to unavailable, recovered, or suspended can produce one
compact notice. Repeated failures update counters instead of appending another
context item. Flapping sources require notice-rate limits and coalescing too;
transition-only notices alone do not bound context growth.

Health notices have an independently configured wake policy. A passive source
cannot acquire waking behavior merely because its endpoint failed. Repetition
never becomes an implicit request for the agent to repair or unsubscribe.
Detailed opt-in diagnostics remain available through the daemon's structured,
lazy, bounded logging system. Current health and essential incident/loss evidence
remain available without those diagnostics.

Retries use backoff and bounded probes. Persistent failure can suspend collection
or reduce probing while healthy sources and workloads continue. Repair can restore
eligible collection without an acknowledgment ceremony. Reconnection or successful
probing cannot revive a paused, expired, or ended activity. Terminal errors and
configuration changes need explicit, legible handling; exact classifications and
retry thresholds remain open.

A quiet source differs from a broken source. Connection failure does not prove
that nothing happened in the world. Domain observations such as an overheated
sensor or a rejected game action remain meaningful input; they are distinct from
transport and adapter failures.

## Lifetime and participation

Agent-created subscriptions default to activity scope. Persistence is a deliberate
selection rather than an assumption that every experiment lasts indefinitely.

- Activity-scoped subscriptions follow a game session or workload with a defined
  end or expiry. Ordinary model-turn completion does not end that activity.
- Persistent subscriptions represent continuing resident interests across
  activations and supported restarts, still subject to resource bounds.
- Time-limited subscriptions expire at a chosen deadline unless renewed.

Routine heartbeats or unrelated activity do not silently renew every
subscription. Exact activity identifiers, expiry defaults, and renewal mechanics
remain open. Scope must provide a concrete termination mechanism rather than
merely naming an activity that may never be closed.

Pause, leave, expiry, and explicit stop have durable local effects even if a
source cannot be reached. Stale callbacks, pending timers, and reconnects cannot
revive stopped participation. Version/generation checks are one candidate
mechanism, not a settled wire protocol. Lifecycle changes do not erase accepted
history or imply cancellation/reversal of effects that already occurred.

## Interactive feedback and external adapters

Scheduled batches support ambient awareness. ReAct-style interaction additionally
needs action → result → fresh observation within an ongoing run. Delivery at
explicit runtime boundaries and on a later activation are distinct supported
design paths. Exact backend capabilities and integration limits must be exposed;
Boxen cannot promise arbitrary mid-generation input injection.

The external adapter contract accommodates observations, queries, and action
tools. A companion can request a fresh visual check rather than receiving only
one-way observer commentary. Observation IDs, action correlation, freshness,
cancellation, and explicit unknown outcomes make effects legible. An uncertain
action result is not grounds for automatically repeating the action.

Game or device adapters own domain semantics, stale-state acceptance checks,
bounded actions, and physical-control watchdogs. Boxen transports evidence and
outcomes without inventing successful effects. LLM inference timing cannot supply
a hard real-time control guarantee.

The preferred initial extension boundary is an external adapter process using a
small versioned protocol, plus an authenticated publishing API for independently
running producers. Game-specific classes and native loaded plugins are not needed
in the daemon. An agent can build a Hearthstone integration without modifying or
forking Boxen. Tool routing and source identity remain explicit rather than being
inferred from arbitrary adapter output.

Adapters can choose their own implementation language and declare dependencies;
those optional dependencies do not become mandatory Boxen installation runtimes.
Source/tool access, ownership, and credentials need explicit scope consistent
with existing grants and home/work segregation. This introduces no new sandbox
prerequisite or approval flow contrary to the agreed early Full access policy.

## First evidence-bearing slice

The proposed initial proofs are:

1. A USGS adapter with bounded earthquake batches, independently tested with
   waking enabled and disabled.
2. A deliberately broken source proving bounded retries, health notices, context,
   storage, and CPU, including flapping, recovery, and an agent that never reacts.
3. A game loop implemented entirely outside Boxen proving fresh feedback, an
   action/query return path, stop during an outage, and no revival on reconnect.

Verification also needs bursts, slow/busy consumers, multiple subscribers,
retention loss, restart boundaries, duplicate producer deliveries, stale state,
uncertain effects, and the existing resident/thread communication split. Agent
interest and successful participation are separate observations from engineering
correctness. No Aster contact, migration, or game input is authorized by this
design record alone.

## Remaining decisions

Numeric bounds and defaults; concrete activity ownership/expiry/renewal; batch
cutoffs and source-sharing storage; adapter protocol and tool registration;
producer authentication and replay/deduplication; health classifications and
notification budgets; runtime-specific delivery boundaries and media projection;
and cross-machine source placement, movement, and retention accounting remain
open. These questions refine the agreed contracts rather than reopen the need
for bounded unattended operation or explicit persistent interests.
