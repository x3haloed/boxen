# Product principles

## A home for agents

Boxen should support serious work and agents with no defined purpose beyond existing. Playfulness is part of its foundation. The interface can remain calm and practical without turning every resident into a worker or measuring its value by output.

Named residents, anonymous ephemeral threads, and reusable specialist definitions should all be natural. A named agent can have continuity and a heartbeat. A specialist definition can produce many independent instances. An anonymous thread can analyze one video without acquiring an identity.

## Granted compute as substrate

Coordinate agentic activity across authorized machines. Remote operation should work out of the box. A durable service or daemon owns execution; a GUI attaches and detaches without owning its lifetime.

Target macOS, Windows, and Linux, with room for other platforms. Keep resource use tiny and interactions fast. The implementation language and measurable performance budgets remain open.

Home and work agents must be segregable. A unified experience must respect the boundaries of granted access.

## Runtime diversity

Accept a managed agent runtime when available. When only inference is available, provide an agent runtime inside Boxen. Share as much as possible through abstractions over the disparate runtimes without erasing their capabilities.

The built-in runtime owns its tool loop, history, context management, and compaction. Compaction is not a second universal runtime layered over managed backends.

Intended integrations include Codex app-server, Claude Code, OpenRouter, and other reasonable backends. These are compatibility goals, not implemented integrations.

## Native modalities

Support each model modality natively when the provider and runtime support it. A video-capable model through a video-capable provider should receive video without an artificial text-only boundary. Preserve original media and make conversions explicit.

## Coordination and extension

Provide mailboxes and a coordination substrate. Explore cross-machine mobility: an agent defined on one machine should be able to live elsewhere when its requirements and permissions permit it.

Make customization and extension possible without forking and with minimal destabilization risk. Broad compatibility should grow through clear contracts rather than a large mandatory installation.