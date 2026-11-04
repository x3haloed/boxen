# Automatic work-item workflow

`workflow.json` beside the service configuration enables an optional project
workflow. The GUI is unchanged; compatible Boxen core supplies the bounded `thread_activity` metadata query. A missing file, or
`"enabled": false`, preserves manual operation. Configuration and project-driver
installation are operator actions; neither a work-item body nor incoming mail can
enable automation, change its project, expand its publication authority or invoke
an arbitrary program.

## Execution and authority

The existing `work`, `assignments` and `reservations` tables retain coder identity,
assignment versions, capacity and resource ownership. New workflow tables retain
source-version attempts, typed requests/responses, question intents and publication
state. They do not create another worker-ownership or resource authority.

The normal path is:

1. Fresh eligible mirror evidence produces one durable interpretation request to
   the configured context resident. Original source is retained in a hashed file;
   historical ownership and previous delivery remain explicit.
2. An authenticated `ready` decision supplies concrete acceptance scenarios,
   preservation targets and inspected evidence. Open holds/questions and unresolved
   ownership stop that item's implementation. The decision alone grants no copy or
   publication authority.
3. An allowed new copy or the existing bound coder receives an immutable assignment.
   Existing capacity/resource reservations precede workspace preparation and model
   dispatch. Queued attempts do not instantiate coders ahead of capacity. Paused
   manual work is not silently resumed; legacy ownership checks remain in force.
4. The coder performs the normal full local repository/database/application/browser
   workflow, with the shared instructions and allocated resources. A structured
   candidate report pins commit/base and observed results for every assigned
   scenario. Builds or private final narration do not substitute for this handoff.
5. The configured context resident and a distinct technical-reviewer copy receive
   separate exact-commit review requests. Findings return to the same coder through
   a new retained assignment version. A new review round has new stable request IDs;
   stale approvals and self-review cannot satisfy the current gate.
6. Successful required reviews, current source, source hygiene, independently read
   cleanup state and configured publication authority permit a non-force push and
   regular PR creation. Intent is committed before creation. Remote readback checks
   repository, source/target, exact source commit, non-draft state, work-item link and
   absence of autocomplete. An externally completed PR can also satisfy readback.
7. The user receives an explicit publication or blocked-state message. Implementation
   cleanup frees capacity only after run/mail and project-driver readbacks. The
   service-owned browser/profile remains retained for later jobs.

Source revisions coalesce while a version is active. An invalidated version cannot
publish; existing effects/evidence remain retained and its coder remains bound.
Source observation outages wait for fresh evidence rather than inventing a new job.
A fair bounded scan avoids starving later attempts; unchanged source observations do
not repeatedly rebuild complete history packets. The mirror retains its configured
poll interval.

A configured `post_clarifications` flag allows questions explicitly routed to
`work_item` to become attributed automated ADO comments. Stable question IDs and
remote marker readback prevent routine reposts. An `existing_comment_id` references
an original question already posted. Operator/support questions remain mailbox
notifications. Answer references retain the exact original comment/version or
requirement revision/field and require an author in the configured decision role;
a service-authored question does not become an authority answer.

## Response contracts

All responses are JSON bodies sent through explicit Boxen mail to the authenticated
request `reply_to`, with envelope `in_reply_to` equal to the request message ID.
Every decision echoes `request_id`, `attempt_id`, `work_item_id`, `source_digest`
and the response protocol included in the request. Sender and destination are
validated against the retained request and workflow client, not body claims.

`boxen.interpretation.v1` supplies `decision` (`ready`, `clarification_required`,
`no_action`, `superseded`), `intended_outcome`, `scenarios`, `preservation_targets`,
`questions`, `holds`, `existing_work.unresolved_ownership`, and `evidence_refs`.
Each scenario has a stable `id`, role, starting state, action and expected result.
Optional `authoritative_answers` reference frozen ADO evidence and a configured
`authority_role`; they do not replace that original evidence with a paraphrase.

`boxen.review.v1` echoes `role` (`context` or `technical`), `reviewed_commit` and
`base_commit`, then supplies `decision` (`pass`, `changes_required`, `blocked`)
and `findings`. Each finding has a stable ID, `blocking`, evidence and a specific
requested change. A pass has no blocking findings and covers only its exact commit
and role. It does not certify browser observations performed by another actor.

The existing `boxen.work-report.v1` candidate includes full `commit` and `base`,
normal summary/evidence/remaining/cleanup fields, and
`validation.local_runtime_exercised: true`. `validation.acceptance` contains an
`observed_pass` result and retained evidence for every assigned scenario ID.
These are attributed worker observations; the independent reviews assess their
substance. Allocation inspection explicitly does not claim runtime verification.

## Configuration

A private configuration supplies `enabled`, `allow_new_copies`, `publication`,
`post_clarifications`, recorded human `authority`, the context resident ID,
`review_definition` and `review_revision`, a typed `notify_to`, and optional `items`
allowlist. An empty allowlist covers the mirror's normal eligible items. Capacity
continues to come from the existing service configuration.

The trusted project adapter uses absolute `driver` and `driver_script` paths,
`driver_args`, and the script's UTF-8 SHA-256 `driver_hash`. Project configuration
includes the repository identity/origin/target, matching Azure organization/project,
workspace/evidence roots, runtime allocation recipe, persistent browser profile and
configured decision-author identity IDs. Credentials stay in their normal CLI/OS
stores, outside requests, profiles and logs. Deployment identities, real names and
local environment values belong in private configuration, not this public repo.

`Project-Driver.ps1` is the Windows PowerShell 7 adapter. Its `plan`, `prepare`,
`verify_cleanup`, `inspect_candidate`, `push`, `find_pr`, `create_pr`, `verify_pr`,
`find_question` and `post_question` operations use structured request/reply files.
The Rust service verifies the configured script hash and bounds execution/output;
its owned process tree is stopped on deadline. Driver replies and exact request
files are retained under the service home. The driver never force-pushes, resets
source, kills application processes, copies credentials or deletes a database.
Application setup/cleanup remains assigned coder work; the driver checks the result.

`status` includes workflow attempts. `workflow` advances one explicit operator tick
and returns JSONL transitions followed by a snapshot; it is not a read-only status
command. The normal service worker advances enabled workflows independently of the
GUI. Stopping automatic transitions does not cancel already accepted model work.

## Recovery and limits

Stable mailbox IDs reconcile uncertain sends without another copy or assignment.
An unknown PR/comment create outcome enters read-only reconciliation; absence of a
visible result does not authorize a second create. Remaining uncertainty is visible
and blocks that item. No acknowledgment ceremony gates unrelated mail or work.
Repeated transport errors back off and retain metadata; malformed or stale decision
messages cannot advance the workflow. Review/interpretation deadlines produce
explicit blocked outcomes, not inferred success. Unchanged candidates returning the
same unresolved findings escalate rather than cycling indefinitely.

Manual pauses, historical ownership transfers, real product/access decisions and
irreducibly uncertain external effects can still require operator support. This is
an automatic development-to-PR loop, not automatic merge/deployment or authority to
edit arbitrary project data. The supplied driver targets one local Windows project;
cross-host allocation and other project runtime recipes are separate adapters.
No deployment or staging acceptance is inferred merely from a merged PR.

Verification includes the existing supervision/assignment suites and new workflow
protocol fixtures for correction, source invalidation, capacity, forgery, restart and
lost PR-create response. `tests/project-driver-smoke.py --pwsh ABS_PWSH` uses real
local Git and native Azure/SQL fixtures to exercise the Windows driver without
cloud writes. These checks remain distinct from a real model/application pilot.
