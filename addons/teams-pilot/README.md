# Teams desktop pilot

These macOS probes use a disposable local Boxen daemon and fresh Codex residents.
Teams uses the account already signed in on the desktop. The probes use Boxen's
existing observation and workload control APIs.

The incoming probe watches one configured chat-list row using the visual sensor.
A changed message preview can wake the resident, whose output is private
narration. The outgoing probe explicitly activates a different resident for one
synthetic message to the signed-in user's self-chat.

## Setup

Use a live macOS desktop with Teams signed in and its chat list visible. Enable
chat-list message previews and timestamps. The OCR crop is calibrated for the
pilot layout. The sensor needs Screen Recording permission. The outgoing probe
also needs a working Codex Computer Use installation and its macOS permissions.
The signed-in Codex executable is discovered on PATH.

From the repository root:

```sh
cargo build --locked --release
mkdir -p work/teams-pilot
xcrun swiftc -parse-as-library -swift-version 6 -O \
  -target "$(uname -m)-apple-macosx14.0" \
  addons/visual-sensor/VisualSensor.swift -o work/teams-pilot/visual-sensor
```

## Incoming preview

```sh
python3 addons/teams-pilot/live-wake.py --chat 'Example Person'
```

Use the exact visible chat label. For a self-chat, add `--self-chat` to include
previews prefixed with `You:`. The first readable row establishes a baseline.
Once `status.json` shows a readable row and an acquired baseline, a new message
with distinct preview text can exercise the wake. The probe runs for 15 minutes
by default. Use `--ocr-label` for a known OCR spelling of the label or
`--baseline-capture` to seed the baseline from an earlier visual-sensor JSONL
capture.

## Outgoing self-message

```sh
python3 addons/teams-pilot/outgoing.py --self-chat 'Example Person (You)'
```

This command requests an actual Teams send. The probe answers scoped Computer
Use permission requests through Boxen's control API. The app and operation
allowlist applies to the permission request. The resident checks the self-chat
and composer in the GUI, and the probe retains the tool events and result. The
permission request carries no verified recipient.

Both probes print their evidence directory under ignored `work/teams-pilot/`.
That directory can contain chat previews and Teams UI state.

## Verification boundary

A live self-chat preview woke a temporary Codex resident once. A separate
explicit activation sent a synthetic self-message that appeared as Sent in Teams.
A single activation that starts from an incoming preview and sends a reply,
deterministic recipient enforcement, and recovery across ordinary interruptions
remain to be tested. A chat-list preview can be truncated, misread, or
overwritten between samples.
