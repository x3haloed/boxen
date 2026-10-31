# Product principles

## A home for agents

Boxen should support serious work and agents with no defined purpose beyond existing. Playfulness is part of its foundation. The interface can remain calm and practical without turning every resident into a worker or measuring its value by output.

Named residents, anonymous ephemeral threads, and reusable specialist definitions should all be natural. A named agent can have continuity and a heartbeat. A specialist definition can produce many independent instances. An anonymous thread can analyze one video without acquiring an identity.

Conversation structure is configurable per agent: one canonical conversation continuing indefinitely, multiple independent conversations, or branches with an explicit reintegration process. A backend session ending or compacting does not end the Boxen conversation.

## Granted compute as substrate

Coordinate agentic activity across authorized machines. Remote operation should work out of the box. A durable service or daemon owns execution; a GUI attaches and detaches without owning its lifetime.

Target macOS, Windows, and Linux, with room for other platforms. Keep resource use tiny and interactions fast. The initial daemon uses Rust with a Cosmopolitan APE build path for an identical-file portable executable. Measurable performance budgets and broader platform verification remain open.

Home and work agents must be segregable. A unified experience must respect the boundaries of granted access.

Identity, memory, conversation history, and deliberately retained durable files travel with an agent. Destination state must be ready before execution switches machines. Portable files have an enforced configurable quota; scratch files stay local by default.

Movement authority is configurable. Users can require explicit direction or grant agents standing permission to move among eligible machines. Model and runtime selection are separate configurable permissions.

## Runtime diversity

Accept a managed agent runtime when available. When only inference is available, provide an agent runtime inside Boxen. Share as much as possible through abstractions over the disparate runtimes without erasing their capabilities.

The built-in runtime owns its tool loop, history, context management, and compaction. Compaction is not a second universal runtime layered over managed backends.

Intended integrations include Codex app-server, Claude Code, OpenRouter, and other reasonable backends. These are compatibility goals, not implemented integrations.

Early versions use full execution access, including Full access for Codex app-server threads. Sandboxing is not an early implementation prerequisite. Full access on a host does not automatically grant movement to other hosts or distribution of credentials. Execution access settings and a comfortable sandbox implementation may be revisited later.

## Native modalities

Support each model modality natively when the provider and runtime support it. A video-capable model through a video-capable provider should receive video without an artificial text-only boundary. Preserve original media and make conversions explicit.

## Coordination and extension

Provide mailboxes and a coordination substrate. Support cross-machine mobility: an agent defined on one machine should be able to live elsewhere when its requirements and permissions permit it.

Make customization and extension possible without forking and with minimal destabilization risk. Broad compatibility should grow through clear contracts rather than a large mandatory installation.

The first implementation proves remote anonymous workloads. Resident continuity, synchronization, configurable conversation structures, reintegration, and mobility remain requirements for beta even though they arrive later. See [agreed decisions](decisions.md) for the scope and acceptance criteria.
