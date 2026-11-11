# Visual sensor

A standalone macOS screen sensor. It samples the primary display or an application window with ScreenCaptureKit, runs Apple's Vision OCR on profile-defined regions, and emits versioned JSON Lines. It can run on a different machine from the agent. The executable needs macOS 14 or later, an active graphical session, and Screen Recording permission. Building requires the macOS Swift toolchain. The initial implementation was exercised on Apple silicon.

## Run locally

From the repository root:

```sh
mkdir -p work/visual-sensor
xcrun swiftc -parse-as-library -swift-version 6 -O \
  -target "$(uname -m)-apple-macosx14.0" \
  addons/visual-sensor/VisualSensor.swift -o work/visual-sensor/visual-sensor

umask 077
work/visual-sensor/visual-sensor capture addons/visual-sensor/display.json 10 \
  > work/visual-sensor/observations.jsonl
```

A frame count of zero captures until interrupted. Adding an existing archive root as the last argument retains the last twelve sampled frames in a private directory named after the session. Images are PNGs of the sampled display, up to 1920 pixels on the long edge. Archive directories persist after exit and can be removed when the session is finished. With no archive argument, the sensor stores no images. Observations can still contain screen text.

A consumer can retrieve any retained frame using the session and frame from the observation. Supplying the original profile and a field ID retrieves that region.

```sh
visual-sensor inspect ARCHIVE_ROOT SESSION FRAME crop.png PROFILE FIELD
visual-sensor replay PROFILE first.png second.png
```

Replay runs the same OCR and tracker over a known sequence without requiring screen permission. Its frame numbers and timestamps identify the replay.

## World of Warcraft

```sh
work/visual-sensor/visual-sensor capture-window addons/visual-sensor/wow.json 24 \
  com.blizzard.worldofwarcraft
```

`capture-window PROFILE FRAME_COUNT BUNDLE_ID [ARCHIVE_ROOT]` selects the largest on-screen standard window for the bundle ID when it starts. It captures that window independently of other apps and does not send game inputs.

The WoW profile samples the quest objective tracker, minimap zone label and numeric coordinates, and target name. It is calibrated for the tested windowed HUD layout, with the quest tracker on the right and the target frame at the upper left. UI scale, window shape, addon placement, or game language can require region or pattern changes. Coordinates require a visible numeric coordinate readout. Objective readings include only OCR rows with an explicit fraction and label. Missing rows, hidden targets, and completed quests can all affect what the HUD shows. A row disappearing does not establish its outcome, and a full counter describes that objective alone.

## Profiles and observations

A profile specifies a target interval in milliseconds and up to eight named regions. Region coordinates are normalized to the captured image, with the origin at the upper left. A field without a pattern returns its OCR text. A field with a regular expression returns the first capture group, or the whole match if there are no groups. Setting `matchAll` joins the matching values with newlines. A quest counter can match `Checkpoint\s+(\d+/\d+)` and require two consecutive confirmations. Profiles are local operator configuration.

OCR defaults to the fast recognizer and original crop size. `ocrScale` can upscale small HUD text by an integer factor up to four, bounded by the capture size limit. `accurateOCR` selects Vision's more expensive recognizer for a field. The WoW profile uses both settings because the fast pass made digit and label errors on the sampled HUD.

Each `boxen.visual.v1` record carries a session ID, increasing frame number, capture time in Unix milliseconds, image dimensions, timing, a current field snapshot, and any newly confirmed changes. An unchanged region reuses the OCR result if its rendered pixel bytes match. Each sample still counts toward confirmation because it is a fresh capture. A pending or unreadable field keeps its last confirmed value and evidence frame, with its status marked `confirming` or `unreadable`. A reader can distinguish the latest attempt from the last confirmed observation. The first confirmed value is an acquisition, represented by a change with no previous value.

The capture and processing times are measured separately. The target interval includes archive encoding and output. Sampling is sequential, with no accumulating frame queue. A slow stdout reader slows sampling. Frames describe sampled states, so a transition that occurs entirely between samples can be missed. Archive lookup can fail after a frame ages out, including when that frame is still cited by a stale field.

## Connection to Boxen

The local sensor owns capture, OCR, tracking, and short-lived evidence. JSON Lines is the process boundary. A separate consumer can forward records using its chosen authenticated transport. The current CLI provides stdout and local frame lookup. Boxen ingestion, remote evidence lookup, and platform-specific capture backends beyond macOS are future work.

For a Boxen adapter, route coalescible field snapshots to current-state observation sources and confirmed transitions to ordered sources. Use session and frame for provenance and duplicate detection. A profile can specialize a game or application without adding its behavior to the Boxen daemon. Activation remains a subscription decision. A gameplay loop can react to a counter change, request the supporting crop when uncertain, and use a separate action path. Capture loss, transport loss, and unreadable fields represent different conditions.
