# Guest Chat

The [Boxen Guest Client](../guest-client/README.md) gives one invited guest a
native desktop conversation with one Boxen resident. The host runner and
client make outbound connections to Google Cloud Pub/Sub using separate
service-account credentials. Both exchange messages and files, and the
client shows the resident's Ready, Working, or Offline status separately from
its connection status.

## Set up a conversation

Use a dedicated resident with automatic activation enabled. Provision the
Pub/Sub resources and IAM grants, then prepare separate host and guest
settings as described in the [client setup guide](../guest-client/README.md).

With the daemon running, create a connector from the same Boxen data directory.
The command prints JSON containing `definition.id`. Keep that ID to restart
the runner or reuse the same guest conversation. Both commands accept
`--url http://127.0.0.1:PORT` for a custom daemon control address.

```sh
boxen guest-chat-create RESIDENT_ID
boxen guest-pubsub --instance CONNECTOR_ID
```

Keep the daemon control API private. Give the guest the client, their own
service-account JSON key, and the three connection details from the setup guide.
The host's credential and daemon control token stay on the host. Use a new
connector and Pub/Sub channel for a different guest so their history and
access are scoped to their own conversation.

## Delivery and resident replies

Boxen retains accepted guest messages and explicit resident replies in the
correspondence journal. A local acceptance receipt records durable acceptance
in Boxen. The runner publishes replies to Pub/Sub and acknowledges guest
input after local acceptance. The client keeps its own history and retrieves
pending deliveries when it reconnects, within the subscription's retention
window. The guest sees only the bound conversation. Disabling its connector
suspends its chat operations and retains the journal.

Resident replies use `boxen_messages` with the incoming message's return
address and an optional `in_reply_to`. The connector advertises `send_files`
in the tool's `list` response. To send files, use `action: "call"`,
`operation: "send_files"`, the guest address in `to`, and
`arguments: { "paths": ["relative/or/absolute/path"], "body": "Optional caption" }`.
Relative paths resolve from the resident's working directory. Boxen retains
originals before accepting the reply. Incoming guest files are available to
the resident through Boxen's standard attachment context.

A managed Codex resident needs `boxen_messages` registered in its native
session. Guest Chat grants conversation access. The resident's own backend,
workspace, credentials and tools determine its execution access.
