# Named Chat Completions endpoints

Boxen can run its built-in inference loop against an operator-configured model
server on the executing machine. The server provides streamed Chat Completions.
Boxen owns conversation history, tools, run control, media snapshots, and
residents and their mail. `openrouter` remains the provider ID for OpenRouter
and uses its own credentials, routing, reasoning effort, and native PDF
configuration.

## Configure the executing daemon

`serve --chat-endpoints PATH` reads a private host-local JSON file at startup.
Each key names a service on that machine. This example describes a vision
service and a text-only service on separate operator-selected ports.

```json
{
  "vision": {
    "endpoint": "http://127.0.0.1:42800/v1/chat/completions",
    "default_model": "example-vision-model",
    "images": true
  },
  "text": {
    "endpoint": "http://127.0.0.1:42801/v1/chat/completions",
    "default_model": "example-text-model"
  }
}
```

The URL uses an explicit IPv4 loopback port and API path. Requests bypass HTTP
proxies and carry no authorization. The daemon selects only names supplied by
its operator. A peer can select the configured backend ID and model but cannot
supply or override the URL. The server's accepted models and its process
lifetime are operator-managed. Keep model files, server working files, and
real endpoint configuration in local storage.

Authenticated `runtime_info` lists each configured `chat-completions:NAME`
alongside managed runtimes and OpenRouter. `configured` and the `input` list
reflect the local declaration. The endpoint's response establishes whether it
is reachable and whether the model supports the requested operation.

```sh
boxen start HOST:42424 PEER_KEY DESTINATION_CWD - 'Describe the image in sample.png.' --backend chat-completions:vision
boxen resident-create HOST:42424 PEER_KEY 'Local reviewer' DESTINATION_CWD - --backend chat-completions:vision
```

In the authenticated API, `start.spec.backend` and
`resident_create.spec.backend` take the same IDs. An empty model selects the
endpoint's default at first acceptance. The run records that resolved model
separately from the original request. Subsequent defaulted runs in the
conversation retain the captured model. An endpoint name is part of the
conversation's backend choice. A move to another host requires the same name
there. The operator is responsible for configuring an equivalent service.

## Wire contract

The endpoint receives deterministic history and tools at the start of the
prompt, followed by the current Boxen-authored system context at the end. An
endpoint's template must support that system message position. Tool replies
use ordinary Chat Completions tool messages. When enabled, image snapshots
follow their owning tool-result group in labeled content parts. The endpoint
must support inline `image_url` data URLs at their original message positions.
`boxen_read` supports UTF-8 text on both profiles. A vision profile additionally
admits PNG, JPEG, GIF, and WebP snapshots according to the existing bounded
reader. Unsupported native modalities return a tool error. Actual image format
support can vary with the endpoint.

The server returns SSE with a terminal finish reason and `[DONE]`. Boxen
retains observed backing frames and uses supported completed projections for
continuity and tool execution. Provider-specific routing, OpenRouter cache
session IDs, native PDF plugins, and OpenRouter reasoning settings apply to
`openrouter`. A named service has its own generation defaults. Shared Boxen
output, message-attempt, exchange, and context limits still apply.

Cancellation closes Boxen's exchange and prevents dispatch of its unfinished
tool calls. The server determines whether disconnect also stops generation.
Termination and subsequent service health require server-side verification.
An unavailable endpoint or interrupted stream is visible in the run and can
be followed by a new run through the existing inference continuation API.

## Verification

`scripts/chat-completions-smoke.py BINARY` exercises paired daemon discovery,
routing, text and image profile isolation, SSE replay, exact retained image
bytes, OpenRouter policy compatibility, and interruption and recovery through
a controlled provider. Pass `--live-endpoint URL --model MODEL` to exercise a
real server with synthetic images, tool calls, daemon restart, and cancellation.

The native macOS live test with Bonsai verified a structured `boxen_read`
call, correct shape, color, and count on two synthetic images, and a follow-up
after deleting the source image and restarting the daemon. During streamed
cancellation, the model service closed its generation iterator after the
connection ended. A later request completed. Those server-side cancellation
results apply to the local Bonsai serving adapter used in the test.

The PrismML Bonsai 2.5 27B MLX checkpoint uses the vendor's bundled loader for
packed language weights and the FP16 vision tower. A local serving adapter
can render each retained image at its message position, render the trailing
system context, and stream decoded output and complete calls using the
checkpoint's tool format. The endpoint runs separately from Boxen so the
portable daemon does not need Python, MLX, or model weights. See the
[PrismML release](https://prismml.com/news/bonsai-2-5-27b).
