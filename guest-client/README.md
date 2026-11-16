# Boxen guest client prototype

The prototype opens a native window and echoes messages in memory. It exercises
the portable window backend, text input, Enter to send, and Shift+Enter to insert
a newline. The production transport and resident mailbox binding are pending.

From this directory, `cargo run --locked` launches the host-native build.
To build the portable artifact, use Boxen's pinned Cosmopolitan toolchain cache
and run `cargo build --locked --release --features ape`. The artifact is
`target/cosmo/release/boxen-guest-client.com`. The portability check launches
copies of the same artifact on Windows and macOS, checks that a window opens
and displays a typed message and its echo, and closes the window normally.
Windows can launch a byte-identical copy with an `.exe` extension. On macOS,
run `bash ./boxen-guest-client.com` from the directory containing the artifact.

On Apple Silicon, the APE opened a native window. Physical input verified
Shift+Enter and Send with local echo. With the `softer_gui` 5.0.1 close fix,
clicking the red close button ended the APE process with exit status zero.

The [pilot infrastructure](infra/main.tf) defines two service accounts and two
directional Pub/Sub topics with one pull subscription each. Each service
account receives Publisher on its outgoing topic and Subscriber on its
incoming subscription. Unacknowledged deliveries remain available for 31 days.
The subscriptions stay provisioned during inactivity. The infrastructure
expects the IAM API and provisioning credentials in the selected project.
Its output lists resource names, and credential issuance is separate.

The pilot checks that the receiver catches up after it has been offline, saves
each delivery before acknowledging it, and uses the same message ID to handle
redelivery. The 31-day window begins at publication. Local history owns
acknowledged messages, and attachments need a matching retention policy.
