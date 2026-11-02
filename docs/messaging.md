# Messaging connectors: first slice

Boxen mail is the bundled `boxen` connector. External services use the same
resident intake and explicit-send contract through **configured connector
instances**. The daemon owns accepted correspondence, activation presentation and
external dispatch receipts; connector runners own native service protocols and
their own credentials. There is no automatic conversion of private narration to
outgoing communication. Anonymous workload threads retain request/response tools.

## Type, instance, account and binding

These are separate concepts:

- A provider/type names an implementation, such as `discord` or an addon-defined
  name. The daemon does not dispatch external provider-specific code.
- An instance has a stable host-local 64-hex ID, owner, name, expected external
  account, declared capabilities, optional credential-file reference, and optional
  public provider settings. Credentials are not shared globally by provider type.
- A binding connects one native conversation to one resident, independently
  permitting receive and send. Multiple bindings explicitly share an account;
  a resident can participate through multiple instances/accounts.
- An external address is `(instance, conversation)`. Native conversation and author
  IDs are opaque, bounded strings. Equal channel IDs in different instances do
  not share routing or deduplication state. The original native author belongs
  to message provenance, not to Boxen's resident identity namespace.

For example, two instances can place two different bots in the same Discord
channel. They have different credential references, expected account IDs, and
resident bindings. One bot can also serve two residents through explicit bindings.
Reusing the same external account in multiple instances is explicit configuration;
Boxen does not pool connections or deduplicate across those instances. Such
instances still represent the same identity to the external service.

`credential_file` is optional for generic addons. A credential-free source or an
addon using another local authentication mechanism needs no fabricated bot key.
The bundled Discord runner requires a private file containing its bot token and
verifies the token's actual account against the configured account. `settings`
contains bounded, non-secret provider configuration. Agent discovery omits both
settings and credential references. File paths refer to the **runner's** machine;
instance mobility and credential distribution are not implemented.

Instances and their bindings are immutable after creation in this slice, apart
from revision-checked enable/pause. Changing accounts or grants uses a new instance
ID, preserving the meaning of historical addresses and receipts.

## Common API

The authenticated peer/loopback API accepts:

```json
{"op":"messaging","args":{"action":"list"}}
```

The CLI uses the same operation:

```sh
boxen messages HOST:PORT PUBLIC_KEY '{"action":"list"}'
```

`boxen-messaging-v1` is advertised by `runtime_info.messaging`. Resident tools use
`boxen_messages` with `list`, `inbox`, `send`, and `status`. Their actor is the
executing resident; a caller-supplied sender cannot impersonate another resident.
Native managed tools are registered when a new native session is created.
Existing Codex sessions can lack the new tool; there is no implicit session reset
or claim of lossless hidden-state reconstruction. New resident sessions support
it directly, as exercised by the real Discord proof.

Addresses on the common API:

```json
{"instance":"boxen","conversation":"resident:RESIDENT_ID"}
{"instance":"boxen","conversation":"client:OWNER_MACHINE_KEY:CLIENT_ID"}
{"instance":"INSTANCE_ID","conversation":"NATIVE_CHANNEL_ID"}
```

A resident send:

```json
{
  "action":"send",
  "id":"MESSAGE_ID",
  "to":{"instance":"INSTANCE_ID","conversation":"NATIVE_CHANNEL_ID"},
  "body":"Hello",
  "in_reply_to":"BOXEN_INCOMING_MESSAGE_ID"
}
```

`subject` and `in_reply_to` are optional. Boxen mail requires a nonblank subject;
a connector without the `subjects` capability rejects a subject-bearing send.
Native reply references are resolved from retained incoming provenance and checked
against the sending participant, instance and conversation. Caller-generated IDs
are 64 lowercase hex characters. Exact-ID retries return the same accepted
message; changed contents conflict. A tool call without an ID is deduplicated by
its retained run/call source, while a new call can create new correspondence.
Operator/client sends require explicit IDs and client IDs; external sends originate
from bound residents. Subject-free Discord sends do not invent email conventions.

`inbox` reads this resident's shared intake, or an operator-supplied Boxen
`conversation`, after an optional global `after` cursor. Messages retain native
account, conversation, message ID, author, timestamp, prepared native metadata,
and attachment metadata where present. `presentation` and `presented_in` identify
recorded activation input, not comprehension. `delivery` describes local acceptance
or the external service outcome. `status` reads one authorized message by ID.

Capabilities are explicit: `send`, `receive`, `replies`, `subjects`,
`native_metadata`. Unsupported sending/reply/header operations are rejected.
Rendered/downloaded attachments, edits, deletions, reactions, native discovery and
other service-specific operations remain outside this first common interface.

## Configure an instance

Operator/connector actions use the same authenticated API, outside resident tools.
They are unavailable through the resident-scoped tool.

```json
{
  "action":"create",
  "definition":{
    "id":"INSTANCE_ID",
    "name":"Personal Discord bot",
    "provider":"discord",
    "account":"BOT_USER_ID",
    "credential_file":"/absolute/runner-local/credentials/discord-personal.key",
    "capabilities":["receive","send","replies","native_metadata"],
    "bindings":[
      {"conversation":"CHANNEL_ID","resident":"RESIDENT_ID",
       "receive":true,"send":true,"accept_bots":false}
    ]
  }
}
```

`get` takes `instance`; `update` takes `instance`, `expected_revision` and
`enabled`. Creation is exact-ID idempotent. Foreign owners cannot inspect or
configure the instance. Host limits are 16 retained external instances, 32 unique
bindings per instance, eight receiving residents per native conversation, public
settings of 8 KiB, and definition files of 32 KiB. Disabled instances retain
correspondence while rejecting new intake and dispatch claims. Malformed instance
files are preserved and isolated; healthy instances, ordinary workloads and
resident continuity remain usable. An unavailable registry directory is also
isolated at startup; configuration is preserved for repair/reload while ordinary
work and existing resident mail remain available.

## External addon boundary

An addon can run in any language and use the documented API. It does not require
an inference-runtime fork or a daemon provider registration. It uses:

| Action | Contract |
| --- | --- |
| `get` | Reads the owned instance definition and expected account. |
| `cursor` | Takes `instance` and `conversation`; returns the last checkpoint. |
| `ingest` | Takes `instance`, `conversation`, prior `after` cursor (null initially), next `cursor`, and a native `messages` batch. Durable fanout precedes checkpoint acceptance. |
| `outbox` | Returns bounded queued messages addressed to the instance. |
| `claim` | Takes `instance`, message `id`, and fresh `claim_id`; retains uncertainty before returning `claimed:true` and the accepted message/native reply reference. A repeated claim returns `claimed:false`. |
| `complete` | Takes `instance`, message `id`, `claim_id`, outcome `status`, and optional `external_id` and bounded `reason`. Accepted service outcomes require an external message ID. |
| `health` | Reports `state` and optional unavailable `channels`. Discovery shows last report/time and explicit staleness. Health changes produce structured diagnostics, not repeated agent input. |

An intake batch contains at most 32 messages and 192 KiB of prepared JSON. Each
native message has `id`, `author:{id,...}`, and `body`; timestamp, attachment
metadata and other native fields can accompany it. IDs are bounded to 256 bytes.
Resident routing comes from the instance bindings, never from native packet
claims about Boxen recipients. Bot-authored inputs are excluded by default;
`accept_bots:true` explicitly enables agent/bot conversation in a binding.

Intake checkpoints use compare-and-set against `after`. A retained batch digest
makes an exact repeated checkpoint idempotent and conflicting cursor reuse
legible. Partially accepted fanout is reconciled through stable instance/
conversation/native-message/resident IDs before the checkpoint advances. Source
keys use that canonical hash, so opaque IDs containing separators cannot collide.
Acknowledgment means durable intake, not activation, reading, understanding or
reply. Messages arriving during execution stay queued for a later activation.

Body retention is limited to 32768 UTF-8 bytes; prepared native metadata is
bounded to 24 KiB before adding Boxen provenance. Oversized portions become
explicit availability records with lengths/hashes and `retained:false`, distinct
from authored text. Later correspondence remains eligible. Hash labels distinguish
UTF-8 body bytes from serialized prepared JSON; provider-reported metadata is not
proof of original wire bytes. Total request bounds still apply: addons reduce an
oversized native packet to an availability envelope before sending it. There is
no claim that an unretained original remains retrievable from its service.

A local queue acceptance and service acceptance are different stages. External
sends are queued durably, with a 256-pending-message bound per instance. Claiming
retains an **uncertain** outcome before any service effect. If the runner crashes,
loses the claim response, or loses the service response, that message is not
automatically dispatched again. New messages can proceed. Bounded reason codes retain service rejection/transport
causes without exposing response bodies or credentials. `complete` can establish
service acceptance/rejection or retain uncertainty; established outcomes cannot
be relabeled. This favors honest uncertainty and continued availability over
repeating possibly completed effects. It does not provide distributed exactly-once
delivery or prove that a person read a service-accepted message.

The existing append-only `mailboxes/events.jsonl` is the shared correspondence
journal, including intake, cursors, dispatch claims and outcomes. The directory
and `record.mailbox` field names remain for storage/API compatibility; their
meaning now includes external connector intake. Run records still own activation
presentation. Connector definition files own configuration. There is no second
inbox, mutable receipt database, or opt-in-diagnostic dependency. Legacy mailbox
APIs/tool names remain compatibility entry points for existing clients and native
session histories; removing them requires a versioned client/tool migration.

Runners currently have an operator's local control capability and run as trusted
full-access extensions. Instance/owner checks and resident tool bindings are
application routing boundaries, not OS sandboxing. A separate least-privilege
addon credential mechanism, automatic supervision/installation, credential
vault/sharing, mobility, and global correspondence retention quotas remain future
work. Ordinary message/run history retains its existing disk-retention limits;
pending correspondence is never silently evicted as an observation buffer.

## Bundled Discord runner

```sh
boxen --data-dir /path/to/requesting/boxen-home discord-connector \
  --instance INSTANCE_ID --url http://127.0.0.1:42425
```

The foreground runner is a subcommand of the portable binary, using only the same
public API available to addons. `--peer PUBLIC_KEY --address HOST:PORT` routes
through the requesting daemon to a paired execution host. `--once` performs one
cycle for diagnostics/tests. It reads its own private bot-token file, verifies the
bot account, and rechecks changed credentials. Unix credential permissions are
owner-only. Tokens never appear in process arguments, routing records or logs.

This first runner polls configured channel histories every two seconds, honors
Discord's response rate-limit headers and retry durations, and sends text/native
replies. It starts at the current tail, explicitly recording `from_now` rather
than presenting old channel history as new arrivals. Catch-up traverses up to
1000 messages before advancing; an unhandled larger backlog leaves the cursor
intact with a health incident. Native attachment URLs are remote references,
not retained bytes or evidence of model perception. Missing content access is
labeled `empty_or_unavailable` rather than invented from filenames or metadata.
A bot needs appropriate channel access and Discord's message-content access or
an applicable exemption. See [the Discord research](research/messaging-connectors.md).

Per-channel read failures back off independently; other channels and outgoing
work can proceed. Repeated health failures update counters with bounded notices,
not an accumulating resident error feed. Sending disables automatic mention
parsing. Text beyond Discord's supported 2000-character boundary is rejected
without a service request. Replies preserve native message references. Discord's
nonce facility supplements duplicate protection but is not treated as durable,
unbounded idempotency. Gateway push, native edits/deletes, posting files, rich
components, DMs initiated by user lookup, and other Discord operations are not
implemented. The runner uses Discord's official HTTPS API; `--api-base` permits
only an explicit loopback fixture alternative and redirects are disabled.

Registration does not launch a process. The runner remains independent of resident
and GUI lifetimes; an operator can run it under a service manager. Pause is hot;
a running foreground runner waits for reenable. Addon supervision is a later
slice rather than a hidden daemon dependency.

## Verification

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/messaging-smoke.py target/release/boxen
python3 scripts/messaging-smoke.py target/cosmo/release/boxen.com
python3 scripts/test-linux.py
```

The controlled suite uses real paired daemons, native resident dynamic tools,
and two authenticated bot accounts against a controlled Discord HTTP server. It
checks equal native channel/message IDs across isolated instances, explicit bot
sharing, original native authors, generic Boxen mail, a credential-free custom
provider, separator-safe opaque IDs, selective/no-send behavior, subject-free
sends, scoped replies, rate limits, unknown effects, restart without effect replay,
new work after uncertainty, wrong-credential-account rejection, oversized-input
availability, stale revisions, corrupt-instance isolation and registry-directory failure. Fixture output is
not evidence that Discord accepted a request or that a real model selected a tool.
The Linux rotation includes this as its fourteenth controlled suite.

`scripts/messaging-live.py` is an explicit opt-in paid/plan-usage and external-traffic
check. It creates a temporary isolated daemon/resident, posts one greeting through
real Codex and Discord, then waits for an authorized human reply before checking
an explicit native reply and retained provenance. It stops its own test processes
and preserves journals under ignored `work/`; existing residents are untouched.
The first live run completed two real Codex activations with a greeting and linked
Discord reply; incoming human author identity was retained and private narration
created no automatic message. No live Discord credentials enter the Linux rotation.
