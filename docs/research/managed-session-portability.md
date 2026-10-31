# Managed-session portability: unresolved fidelity, not assumed impossibility

Investigated 2026-10-31. Native session movement is **unverified for Boxen**. This is a separate capability from transporting Boxen's observed history or reconstructing context in a new backend session. A single canonical Boxen conversation does not establish that the native runtime session can move intact.

The current evidence does not justify declaring Codex- and Claude-backed residents categorically nonportable. It does justify treating faithful native-session migration as an explicit, versioned capability with a potentially host-pinned fallback. A reconstructed session must not silently satisfy a promise of exact native continuity.

## Claude Code

Current official documentation explicitly describes cross-host resume using either a session store or moving transcript files. The store's key depends on the working directory; subagent restoration additionally requires discovering subkeys. This is evidence of an intended transfer mechanism, not a Boxen integration test. [Cross-host resume](https://code.claude.com/docs/en/agent-sdk/sessions#resume-across-hosts), [external session storage](https://code.claude.com/docs/en/agent-sdk/session-storage).

The supplied local research checkout is the unpacked CLI 0.2.122 from May 2025. It cannot establish support for those current APIs. Before implementation, pin an actual supported CLI/SDK version and exercise its documented mechanism. A displayed conversation or exported text is not automatically a complete native transcript.

## Codex

The inspected source provides disk/thread-ID resume and two explicit import paths. However, `thread/resume.history` is marked unstable and specifically for Codex Cloud, with a warning against other use. `thread/resume.path` is also unstable. These are evidence that native state can be loaded from an artifact, not a stable migration guarantee we should advertise. [Pinned resume parameters](https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/app-server-protocol/src/protocol/v2/thread.rs#L340).

The same revision documents provider admission constraints on existing threads: the saved provider must match the configured provider and remain admissible. Copying a file and changing the resume request's provider does not bypass that validation. Destination configuration is therefore part of readiness. [Pinned provider admission documentation](https://github.com/openai/codex/blob/67f0994b532b8740f14b5b738af7b7b2a915b1d6/codex-rs/app-server/README.md#L322).

Boxen currently retains observed app-server events and reconstructs context after an explicitly unresumable session. That recovery behavior does not prove full native-state export, import, or cross-host fidelity. In particular, the public history projection must not be assumed to contain all opaque reasoning, compaction, sidecar, or attachment state the runtime uses.

## The capability to prove

Three cases need distinct names in placement and recovery:

- **Native-session transfer:** retained native conversation/context state resumes on the destination through a tested backend mechanism. Runtime processes, active shell jobs, sockets, and unrelated host files are outside this claim.
- **Conversation reconstruction:** the same Boxen conversation continues with identity/memory and retained history in a fresh native session, with reconstruction limits disclosed.
- **Pinned execution:** required native continuity cannot be transferred under the selected versions/configuration. Observation and portable history can still work remotely; execution remains on an eligible original host.

These are candidate capability distinctions, not a decision to offer automatic lossy migration. A resident whose continuity expectations require the first case may need the third when the backend cannot deliver it. Do not automatically substitute reconstruction or change the resident's runtime. The beta requirement for transportable conversation history remains in place regardless of native-runtime constraints.

## Two-host verification still required

A meaningful test must create a disposable native session, retain its complete migration artifacts, stop the source execution, and resume on a second host with independently available credentials and matching runtime/provider configuration. The probe should verify:

1. Prior user/assistant content, completed tool calls/results, identity/tool definitions, and compaction state continue as supported by the native format.
2. Required attachment bytes and sidecars are present; host-specific paths are remapped or explicitly unavailable. No assumption that copying the transcript copies its filesystem.
3. No tools rerun merely because a session moved. Active/unknown effects remain recorded rather than retried.
4. Resume actually generates a new turn on the destination, beyond simply displaying imported history.
5. The source cannot also continue that canonical conversation under Boxen's execution authority.
6. Missing assets, expired handles, changed directories, incompatible versions/providers, or unavailable source state produce an explicit capability failure or permitted recovery path.

The present research establishes plausible mechanisms and important constraints. It does **not** establish either exact native fidelity or categorical nonportability. No existing resident was moved, rewritten, or used as a migration experiment.
