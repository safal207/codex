# IPC wrong-thread reproduction

The affected source is astral303/codex PR #18 head
`00ae96124babb37e25f77d568858c693ea9a2afd`. A primary thread A owns the IPC
endpoint while writable thread B is displayed. The incoming event loses A's
identity and submits ordinary user input through B's widget.

The accompanying `ipc-wrong-thread.patch` adds two tests to that exact head.
It exercises real TUI event dispatch, ChatWidget submission, and history
addressing with an in-process server fixture. It does not open a socket or
contact a model. The listener is disabled deliberately; the event is the same
event the original endpoint emits.

## Reproduce on the original head

Use a separate checkout of safal207/codex and official Rust 1.95.0. From the
repository root, in Bash:

```bash
git show fix/ipc-thread-routing:docs/repros/ipc-wrong-thread.patch > ../ipc-wrong-thread.patch
git switch --detach 00ae96124babb37e25f77d568858c693ea9a2afd
git apply --unidiff-zero ../ipc-wrong-thread.patch
cd codex-rs
RUST_MIN_STACK=8388608 cargo --config 'profile.test.package.codex-tui.debug=0' test --locked -p codex-tui --lib ipc_wrong_thread -- --test-threads=1 --nocapture
```

Observed on Linux, 2026-10-11:

| Test | Result | What it establishes |
| --- | --- | --- |
| `ipc_wrong_thread_witness` | Passed | A's incoming message returns `Submitted`, produces a user turn containing the marker, and writes history addressed to B. |
| `ipc_wrong_thread_is_rejected` | Failed at its rejection assertion | The original handler does not reject delivery into the other displayed thread. |

The expected combined result on the affected head is **1 passed, 1 failed**,
with exit code 101. A compiler error is not this reproduction. The first
attempt here was killed at the environment's memory limit; the successful
attempt followed a cleanup of generated TUI artifacts and boxed the server
startup future. The patch includes that fixture adjustment.

## Validate the fix

Remove only the added baseline tests and return to the fix branch:

```bash
git restore -- tui/src/app/tests.rs
git switch fix/ipc-thread-routing
export RUST_MIN_STACK=8388608
cargo --config 'profile.test.package.codex-tui.debug=0' test --locked -p codex-tui --lib send_messages_tests
cargo --config 'profile.test.package.codex-tui.debug=0' test --locked -p codex-tui --lib app::send_endpoint::tests::ipc_ -- --test-threads=1
```

The fixed source passed 9 send/widget tests and 6 App routing/generation/UI
tests. Another 62 composer submission tests and 41 review/recovery tests
passed, for 118 total. The TUI-only debug-symbol override fits the constrained
build environment; test assertions remain enabled.

The second dispatch guard was also deliberately disabled in a temporary local
source mutation. The App suite then produced **4 passed, 2 failed**, at the
assertions for stale operations after a view switch and an endpoint replacement.
This confirms that those regressions detect the missing guard. The mutation
was restored and is absent from the published branch.

## Limits

These checks do not verify real socket lifecycle behavior or Windows runtime
compatibility. Thread and generation binding does not authenticate the sender:
another same-account process can still supply text as user input. Socket
ownership cleanup races and transport denial-of-service are separate issues.
Use the fork only in an isolated test environment. Acceptance or an optimistic
user-message echo does not prove model execution.
