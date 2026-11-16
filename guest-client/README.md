# Boxen guest client

The native client exchanges text with a single Boxen resident through two
Google Cloud Pub/Sub topics. Enter sends and Shift+Enter inserts a newline.
The client shows its connection and queue status. Resident presence and
attachment transfer are outside this text transport slice.

From this directory, `cargo run --locked -- --loopback` opens a local-only
window with in-memory echo for input testing.
To build the portable artifact, use Boxen's pinned Cosmopolitan toolchain cache
and run `cargo build --locked --release --features ape`. The artifact is
`target/cosmo/release/boxen-guest-client.com`.
Windows can launch a byte-identical copy with an `.exe` extension. On macOS,
run `bash ./boxen-guest-client.com` from the directory containing the artifact.

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

On the Mac, the default config is `~/.boxen-guest/guest-pubsub.json`.
Start the client with the matching local configuration. It retains its journal
in the `history` directory next to that configuration, or the directory chosen
with `--data-dir PATH`. Use one active client per incoming subscription.

Both sides acknowledge inbound messages after durable local acceptance. A
retry uses the same Boxen message ID, including when a cloud publication
succeeded just before a local restart. Outgoing client drafts become durable
queue entries when sent. Acknowledged messages remain in the local journals;
a fresh installation without that journal can retrieve only pending
deliveries.
