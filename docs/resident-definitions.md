# Resident definitions and independent instances

A resident definition is a reusable, versioned template. Instantiating it creates an ordinary resident with its own ID, canonical thread, identity, mutable memory and mailbox. Instances inherit initial values; they do not share mutable continuity. Creation starts no inference. Instances remain present after their workloads finish. This slice adds no archive/delete or automatic cleanup.

The first surface is the authenticated CLI/API, without new GUI controls or agent tools. Definitions are host-local and owner-scoped. Cross-machine synchronization, shared mutable memory and multiple conversations of one identity remain separate work.

## CLI

`boxen resident-class HOST:PORT PUBLIC_KEY JSON` sends the same authenticated operation as other peer commands. Definition and instance IDs use ordinary Boxen hexadecimal ID validation. These illustrative IDs are distinct:

```sh
boxen resident-class HOST:42424 PEER_KEY '{"action":"create","spec":{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"Researcher","cwd":"/absolute/workspace","identity":"A human-authored identity","memory":"Initial notes","backend":"codex","auto_activate":false}}'
boxen resident-class HOST:42424 PEER_KEY '{"action":"instantiate","spec":{"id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","definition_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","definition_revision":1,"overrides":{"name":"Researcher one"}}}'
```

The returned instance works with existing resident, messaging, activation, scheduling and observation APIs. With `auto_activate: false`, mail waits for explicit or scheduled activation. Its default is true when omitted.

A definition uses existing resident creation fields: `id`, `name`, `identity`, `memory`, `backend`, `cwd`, `model`, `context`, `quota_bytes`, and `auto_activate`. Defaults and validation match resident creation. `context` uses the existing context-policy configuration. A definition's absolute working directory need not exist until instantiation; instance overrides can select an available directory. No credentials, connectors or schedules are cloned.

A definition update creates a new immutable revision:

```json
{"action":"update","id":"DEFINITION_ID","expected_revision":1,"patch":{"identity":"Revised human-authored identity","memory":"New seed notes"}}
```

Existing instances are unchanged. Instantiation always names an explicit revision, and `overrides` can replace any template field except `id`. Repeating the exact initial instantiation request returns the current evolved instance, without resetting memory or creating another mailbox, even if its definition later becomes unavailable. Reusing an instance ID with different inputs is rejected.

## Manual application

Applying a revision requires an exact target definition revision, an explicit nonempty field list and the current **instance** revision:

```json
{"action":"apply","resident_id":"INSTANCE_ID","definition_revision":2,"expected_revision":4,"fields":["identity"]}
```

Only selected fields are replaced. Memory survives unless `memory` is selected; selecting it replaces the instance's memory with the definition snapshot. This is an explicit replacement, not a merge. A stale instance revision, invalid quota or invalid destination rejects the entire operation without partial changes. The source must be the instance's originating definition.

`backend`, `cwd` and `model` may change before the instance has accepted its first run. Afterward, differing values are rejected to preserve the existing conversation binding. Identity/memory changes affect subsequent accepted snapshots; an already running activation keeps its accepted context and can read current continuity through its existing tool. An explicit change to `auto_activate` uses normal mailbox activation behavior.

Instances expose a `definition` association with `id`, `instantiated_revision`, `initial_overrides`, and `last_applied_fields`. The latter maps inherited or explicitly applied fields to their source revision. It records an application, not a claim of continued equality after independent instance edits. Standalone residents retain no association. Definition edits never trigger propagation in the background.

## API and discovery

Use the normal destination envelope with:

```json
{"op":"resident_definition","args":{"action":"list"}}
```

| Action | Additional inputs | Result |
| --- | --- | --- |
| `list` | None | Metadata for visible definitions plus load issues |
| `get` | `id`, optional `revision` | Full exact revision, or latest when omitted |
| `history` | `id`, optional `after` revision and `limit` | Paginated revision metadata (default 20, maximum 50) |
| `create` | `spec` | Immutable revision 1; exact retries return revision 1 after later edits |
| `update` | `id`, `expected_revision`, `patch` | New immutable revision |
| `instantiate` | `spec`: `id`, `definition_id`, `definition_revision`, optional `overrides` | Independent resident |
| `instances` | `id` (definition ID) | Owner-visible associated residents, even if definition storage is unavailable |
| `apply` | `resident_id`, `definition_revision`, `expected_revision`, `fields` | Atomically updated resident |

Definitions live under `BOXEN_HOME/resident-definitions/ID/REVISION.json`, with atomic private publication. The initial host limits are 64 definitions, 256 immutable revisions per definition and 2 MiB per revision file. These do not replace resident continuity quotas or constitute total disk quotas. Instance state, immutable creation inputs and field provenance stay in the existing resident snapshot; mailbox and conversation journals retain their existing authority.

A damaged definition is reported and isolated. Failure of the definition registry does not prevent ordinary residents or anonymous workloads from running. Original files remain available for repair; repair and restart reload definitions. Existing instances do not depend on loading their original template to activate or preserve continuity. Structured opt-in diagnostics cover definition operations and outcomes without recording identity or memory bodies.

## Verification

`scripts/resident-class-smoke.py` exercises real paired daemons and the controlled Codex backend: independent memory and conversations, immutable revisions, explicit field application, concurrent/idempotent creation, owner isolation, quota/conflict atomicity, runtime pins, restart and definition failure isolation. These checks establish daemon behavior, not live model comprehension. The Linux rotation includes the same suite against the identical portable executable.

On November 2, 2026, the class suite passed on native macOS ARM64, the actual APE on macOS ARM64, and the identical APE on Debian 13 x86-64. All 16 Linux controlled suites, direct LAN streaming/replay and remote cleanup passed. Native Clippy, 27 unit tests, relevant resident/mailbox/scheduler/messaging/import/inference regressions, and native/APE timing and idle-CPU checks also passed. No paid inference or live Discord traffic was used for this slice.
