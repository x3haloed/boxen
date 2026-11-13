# Guest chat

Boxen ships a `guest_chat` messaging connector and a small browser chat page in
the same executable. Each instance serves one guest conversation bound to one
resident on the machine running Boxen. The guest opens a link, signs in through
the tunnel's email gate, and can exchange text messages without installing
Boxen or joining its network.

The runner listens on loopback. It serves the page and two chat operations
through a separate port. Requests cannot select a resident, another connector,
or a daemon operation. Its daemon requests use the existing local connector
envelope with the instance fixed at startup. The daemon's control token stays
on the host. A local process with access to the chat port can use that chat,
so the origin assumes a trusted local machine.

## Try it with one guest

Use a dedicated resident with automatic activation enabled. Its instructions
can explain that guest-facing replies are sent explicitly with
`boxen_messages`, using the incoming message's return address and optional
`in_reply_to`. A managed Codex resident needs a native session with that tool
registered. The resident retains the execution access of its backend, so its
workspace, credentials and tools matter independently of guest chat access.

With the daemon running, create the connector from the same Boxen data
directory. The command prints JSON containing `definition.id`. Keep that ID
for subsequent launches of this same guest conversation. Add
`--url http://127.0.0.1:PORT` to both commands if the daemon uses a custom
loopback control port.

```sh
boxen guest-chat-create RESIDENT_ID
boxen guest-chat --instance CONNECTOR_ID
```

Open [the local chat origin](http://127.0.0.1:42427/) to try it on the host.
The page displays the resident's name, the conversation, and a message
composer. For remote access, run a recent official `cloudflared` in another
terminal on the same machine:

```sh
cloudflared tunnel --url http://127.0.0.1:42427 \
  --http-host-header 127.0.0.1:42427 \
  --allowed-mail guest@example.com
```

On Windows, run the same command on one line in PowerShell:

```powershell
cloudflared.exe tunnel --url http://127.0.0.1:42427 --http-host-header 127.0.0.1:42427 --allowed-mail guest@example.com
```

Use one exact email address. The tunnel should announce one-time PIN
authentication. Share the generated HTTPS link after confirming a fresh
browser session sees the email gate. The link changes on tunnel restart.
Stopping the tunnel ends remote access. Changing the intended guest calls for
a fresh connector instance so the new guest starts with their own transcript.
See [Cloudflare's protected Quick Tunnel documentation](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/#restrict-access-by-email).

For a stable address, configure a named tunnel to the same origin. Set its HTTP
Host Header to the loopback address and port, create an exact-email Cloudflare
Access policy covering the entire hostname before publishing the route, and
enable Protect with Access so `cloudflared` validates the app token. The
guest-chat origin relies on that gateway for remote sign-in. The tunnel routes
to the guest-chat port. Keep the daemon control port private. Both local
processes and the tunnel need to be running for the guest to use the chat.
See [Cloudflare's self-hosted Access setup](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/).

## Delivery and retention

The existing Boxen correspondence journal retains accepted guest messages and
explicit resident replies. A send is available to the guest once Boxen commits
it to that journal, including if their browser is closed. An acceptance receipt
describes that local availability. Read receipts are outside this connector's
contract. Guest messages use stable IDs so a retry from the same open page
returns the original acceptance. Reloading the page reads accepted history
again. Unsent drafts and unresolved submissions live in the open page.

A guest sees the two participants' messages in this connector instance. The
resident's other mail, private narration, runs and tools are outside the
projection. The page polls retained correspondence, so brief disconnections and
runner or daemon restarts preserve the conversation. Disabling the connector
through the revision-checked messaging API suspends chat reads and sends and
retains the transcript.
