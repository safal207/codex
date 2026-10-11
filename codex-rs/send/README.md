# codex send

`codex send` delivers a text message to a running Codex TUI session now. The
session treats it as text typed into the composer and submitted with Enter: it
steers the active turn, or starts a new turn when idle. `codex queue` instead
waits for the current turn to end.

```shell
codex send --thread 01a05483-980f-7f13-80a8-a6efb95dd407 --message "Ghidra is available. You can acquire the lock."
```

The thread id is the session's UUID (`/status` in the recipient shows it). The
command exits 0 when the recipient accepted the message, and prints one of:

| Output | Meaning |
| --- | --- |
| `Delivered message to thread …` | The text entered the turn pipeline, as a steer or a new turn. |
| `Thread … accepted the message and will submit it when it can take input.` | The session is still connecting, paused after a disconnect, or waiting on a reserved turn start. The text is held in the session's follow-up queue. |
| `no live session is listening for thread …` (exit 1) | No session owns that thread, or the recipient opted out. |
| `the session endpoint … is not accepting connections` (exit 1) | A session exited without cleanup. The next session on that thread replaces the stale socket. |

A message that arrives during `/review` or a manual `/compact` is accepted and
submitted when a regular turn can take it. Accepted does not mean the model has
read the text: a steer becomes visible to the model at the same boundary as a
steer typed in the TUI.

The endpoint belongs to the session's primary thread. Delivery is rejected
while a different thread is displayed. Requests and queued messages retain
both that thread id and the endpoint's generation; switching threads or
replacing the endpoint before submission can cancel an already accepted
message. Return to the primary thread before sending, and check for agent
processing or a response to confirm execution. The user-message echo can appear
before dispatch. An acceptance reply is not a guarantee of model execution.

Shell escapes (`!…`) and slash commands (`/…`) in a sent message are delivered
as plain text, not executed. Images are not supported.

## Recipient configuration

Every TUI session listens by default. Opt out in `~/.codex/config.toml`:

```toml
[ipc]
enabled = false
```

The socket is `$CODEX_HOME/ipc/<thread-id>.sock`. Another OS user cannot
connect: the directory is owner-only (0700 on Unix, a user-only DACL on
Windows) and the socket is 0600 on Unix. The socket is created when the
session's primary thread is known (start, `/resume`, `/new`, `/fork`), moves
when the primary thread changes, and is removed on exit. A socket that still
answers a connection is not replaced; the second session logs a warning and
does not listen.

Unix socket paths are limited to about 104 bytes on macOS. `~/.codex/ipc/` plus
a UUID fits; a long `CODEX_HOME` does not, and the session logs
`exceeds the platform's Unix socket path limit`.

A sender running inside a Codex sandbox with network disabled cannot reach the
socket unless `$CODEX_HOME/ipc` is in that sandbox's `unix_sockets`
allowlist.

### Local sender trust

Filesystem permissions and the Windows peer check restrict access by OS
account, not by application. Another process running as the recipient's user
can submit text that enters the ordinary user-turn pipeline. The wire protocol
has no sender authentication or approval step. Thread and generation checks
prevent accidental delivery into another session; they do not establish that
the account owner authorized the message.

Use this experimental fork only in an isolated account or environment without
production credentials or valuable writable files. Keep IPC disabled when
other processes under the same account are not trusted. Existing tool approval
rules still apply, but do not make incoming text trustworthy.

## Wire format

One JSON line each way over the Unix domain socket.

```json
{"id":"<uuid>","text":"Ghidra is available.\nYou can acquire the lock."}
{"result":"submitted"}
```

`result` is `submitted`, `queued`, or `rejected` with a `message`. A repeated
`id` within the session's last 64 requests returns the original result without
a second submission, so a sender may retry a connection that dropped before the
reply arrived. Requests longer than 1 MiB are rejected. A connection that sends
nothing for 5 seconds is closed with `rejected`; the sender gives up on a reply
after 30 seconds.

## Windows smoke test

`codex_uds` uses AF_UNIX sockets on Windows 10 1803 and later, the same
transport as `--remote unix://`.

1. Start a recipient in PowerShell: `codex --no-daemon`. Note the thread id
   from `/status`.
2. Confirm the socket exists: `Get-ChildItem $env:USERPROFILE\.codex\ipc`.
   Expected: one `<thread-id>.sock` entry.
3. Idle recipient. From a second PowerShell:
   `codex send --thread <id> --message "ping from powershell"`.
   Expected: `Delivered message to thread <id>.` and a new turn in the
   recipient.
4. Working recipient. Give the recipient a long task, then send again.
   Expected: `Delivered …`, the running turn is not interrupted, and the text
   is shown as a steer.
5. Review. Run `/review` in the recipient, send a message.
   Expected: `Delivered …`; the message is submitted after the review ends.
6. Multiline. Send `--message "line one`nline two"` (PowerShell backtick-n).
   Expected: both lines in the recipient's transcript.
7. Cleanup. Exit the recipient with `/quit`.
   Expected: the `.sock` entry is gone, and a further `codex send` reports
   `no live session is listening`.
8. Opt-out. Add `[ipc] enabled = false`, start a recipient.
   Expected: no `.sock` entry; `codex send` reports `no live session`.

Known Windows gaps: the liveness probe cannot distinguish a stale file from a
socket on Windows, so a second session relies on the 500 ms probe timeout
alone to detect a live owner. Elevated senders are refused by the peer check
in `codex_uds`.
