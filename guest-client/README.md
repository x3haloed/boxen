# Boxen guest client

The native client exchanges messages and files with a single Boxen resident
through two Google Cloud Pub/Sub topics. The header shows the resident's Ready,
Working, or Offline state and reports connection and queue status separately.
Enter sends and Shift+Enter inserts a newline. The conversation scrolls without
moving the composer.

On Windows and macOS, drop files anywhere in the window or use **Attach files**.
A draft can carry up to eight files of 32 MiB each. Each file can be removed
before sending. Click an image thumbnail to preview it in the window. Other
attachments open a Save dialog. The Windows APE uses an in-app picker for file
selection and saving. The resident sends files through its existing guest-chat tool.

To build the portable artifact, use Boxen's pinned Cosmopolitan toolchain cache
and run `cargo build --locked --release --features ape`. The artifact is
`target/cosmo/release/boxen-guest-client.com`.
Windows can launch a byte-identical copy with an `.exe` extension. On macOS,
run `bash ./boxen-guest-client.com` from the directory containing the artifact.

## Connect the desktop client

Give the guest the app, their Google Cloud service-account JSON file, and one
copyable block with their three connection details. These are the guest
account's outgoing topic and incoming subscription.

```text
project_id=example-project
publish_topic=outgoing-example
receive_subscription=incoming-example
```

On first launch, choose or drop the JSON file, paste the block, and select
**Connect**. The app checks both Pub/Sub permissions before saving setup and
opening chat. It reads the service-account identity from the key and keeps a
private copy in the local settings directory. Future launches reuse those
settings. Launch with `--setup` to configure a replacement connection.

## Configure each endpoint

In Google Cloud, enable Pub/Sub and IAM. Each service account receives
Publisher on its outgoing topic and Subscriber on its incoming pull
subscription. Set both subscriptions to never expire from inactivity and
retain unacknowledged messages for 31 days. The 31-day window begins at
publication. Resource provisioning can be done in the Google Cloud Console.

Both endpoints use the same JSON structure. Each file contains the local
principal, its outgoing topic and its own incoming subscription. Use resource
IDs as displayed in the console. This example contains placeholders.

```json
{
  "project_id": "example-project",
  "service_account_email": "example-endpoint@example-project.iam.gserviceaccount.com",
  "credentials_file": "service-account.json",
  "publish_topic": "outgoing-example",
  "receive_subscription": "incoming-example"
}
```

Relative key paths resolve next to the configuration file. Credentials and
deployment settings belong in private local files. Both programs accept
`--config PATH` and support `--check` to verify authentication and IAM without
publishing or consuming chat traffic.

On the Boxen host, the default config is `guest-pubsub.json` in its data
directory. Create a dedicated correspondence binding using
`boxen guest-chat-create RESIDENT_ID`, then start
`boxen guest-pubsub --instance INSTANCE_ID` as a separate process supervised
alongside the daemon. The runner uses the authenticated loopback daemon API
and makes outbound HTTPS requests to Google. Its first run starts at the
beginning of the chosen binding's journal, so reusing an old binding can
publish historical resident replies into the channel. A fresh binding
starts with an empty conversation.

The client normally stores configuration at `~/.boxen-guest/guest-pubsub.json`.
It retains its journal
in the `history` directory next to that configuration, or the directory chosen
with `--data-dir PATH`. Use one active client per incoming subscription.

Both sides acknowledge inbound messages after durable local acceptance. A
retry uses the same Boxen message ID, including when a cloud publication
succeeded just before a local restart. Outgoing client drafts become durable
queue entries when sent. Acknowledged messages remain in the local journals;
a fresh installation without that journal can retrieve only pending
deliveries. The sender publishes attachment parts on the existing channel.
Each part is retained before acknowledgment. Boxen and the client assemble and
verify the original bytes before accepting the associated message. Incomplete
staging expires after 31 days and completed originals remain in local storage.

The client and host runner use immediate Pub/Sub pulls, with a one-second
pause between successful sync passes. This keeps outgoing messages and presence
probes moving while the incoming subscription is empty. Offline still means no
fresh resident presence has arrived within 45 seconds; a cloud connection alone
does not establish resident availability.

Use updated builds of the host runner and native client together for file
transfer. A text-only client can consume a Pub/Sub delivery it cannot display.

The client embeds [DM Sans](https://github.com/google/fonts/tree/main/ofl/dmsans)
under the SIL Open Font License. The local window backend is based on
[softer_gui 5.0.1](https://github.com/ShieldedLabs/softer_gui/commit/de63f083ea670b4e453710863c83e3d9a03d9906)
with Windows and AppKit file dialogs and drop handling.
