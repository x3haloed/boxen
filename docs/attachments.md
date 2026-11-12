# Original files in conversation

Boxen mail and anonymous workload inputs can carry references of the form
`{"name":"review.txt","size":123,"digest":"<lowercase SHA-256>"}`. The optional
`attachments` array belongs to the mail envelope, run spec, or steer action.
Existing requests and retained records without the field remain valid. Files
are distinct from runtime media snapshots created by an explicit read, and from
connector-specific native uploads.

The authenticated machine operator API provides `op: "attachment"` with
`file` and one of these actions:

| Action | Fields | Result |
| --- | --- | --- |
| `status` | — | `complete` and the retained byte offset `next` |
| `write` | `offset`, base64 `data` | Advances a contiguous transfer, publishes on exact size and digest, returns `complete` and `next` |
| `read` | `offset` | Base64 `data` and `next`, up to 192 KiB of original bytes |

The store is under the daemon data directory. It uses content addressing,
bounded chunks, eight references per message, 32 MiB per file, and a host
limit of 1 GiB or 8,192 staged/retained objects. An incomplete object is never
readable as an original. A transfer resumes from the destination's retained
offset. A digest match lets additional references use the same original.
Files are retained independently of source paths and viewers. This slice
does not garbage collect unreferenced staged originals. Machine operators
already have access to the host's retained workload data; the attachment
operation is not exposed as a connector or resident tool grant.

The sender queues remote resident correspondence in the existing mailbox
journal. The same delivery loop transfers originals before delivering the
envelope; destination acceptance still comes from the destination mailbox.
Anonymous requests transfer files before starting or steering the named run.
A draft or acknowledged send never rereads the original source pathname.
The run journal retains the accepted input's references, not the file contents.

On execution the runtime gets a metadata listing with host paths outside the
working directory. Paths are local to the executing machine. A missing or
damaged retained file can still be identified by its original reference, and
its availability is reported without rereading its source. Managed runtimes
can open paths with their own tools. The built-in runtime's explicit
`boxen_read` supports the formats and sizes documented in
[Native file reading](inference.md#native-file-reading); shell tools can handle
other formats. Those reader limits do not change the attachment transfer limit.

`scripts/attachments-smoke.py` runs against native or APE daemons. It exercises
offline cross-machine mail, restart, exact retries, corrupt and missing file
rejection, path-only managed start and steering, and a built-in runtime read
that finds bytes absent from the original model input. The backends in this
test are controlled fixtures; it does not measure real model perception.
