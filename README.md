# vmux

`vmux` is a small tmux companion UI with a **recent activity sidebar**.

The core idea is simple:

- run `vmux` to choose a tmux session and see recently active panes
- have automation append `notify` events when a pane needs attention
- append `clear` events when that pane no longer needs attention
- let `vmux` reconstruct the sidebar by replaying the event log

This makes `vmux` useful as a bridge between **interactive tmux work** and **background automation** like agents, watchers, deploy scripts, and cron jobs.

## What vmux does

`vmux` has three modes:

- `vmux` — open the interactive UI
- `vmux notify <payload-path>` — append a recent-activity event
- `vmux clear <payload-path>` — remove a recent-activity entry for a pane

Recent activity is keyed by **`(session_id, pane_id)`**.
That means your scripts should target a real tmux pane, not just a session name or a window title.

## Build and run

```bash
cargo build
./target/debug/vmux
```

Or install it locally:

```bash
cargo install --path .
vmux
```

## Interactive UI

Run `vmux` with no arguments:

```bash
vmux
```

It will:

1. list tmux sessions
2. show recent pane activity in the sidebar
3. attach to the selected session
4. optionally focus the selected pane

If you use a non-default tmux socket, set:

```bash
export VMUX_TMUX_SOCKET=my-socket
vmux
```

## Event log and configuration

### Event log path

By default, `vmux` stores recent activity in:

```text
~/.cache/vmux/recent-activity.jsonl
```

Override it with:

```bash
export VMUX_RECENT_ACTIVITY_LOG_PATH=/path/to/recent-activity.jsonl
```

This is useful when:

- you want one log per tmux server
- you want a project-local log for experiments
- you want a dedicated log for an ops/dashboard socket

### tmux socket

When your scripts talk to a non-default tmux server, use the same socket consistently:

```bash
export VMUX_TMUX_SOCKET=my-socket
```

Use that env var both when:

- running `vmux`
- querying tmux for `session_id` / `pane_id`

### PTY / wrapper environments

If you launch `vmux` from a wrapper that needs a terminal type, `xterm-256color` is the safe choice:

```bash
export TERM=xterm-256color
```

## Subcommand reference

### `vmux`

Open the interactive UI.

```bash
vmux
```

### `vmux notify`

Append a `notify` event.

```bash
vmux notify payload.json
# or
vmux notify --payload-path payload.json
```

Payload format:

```json
{
  "sessionId": "$1",
  "paneId": "%3",
  "paneDisplayText": "tests failed",
  "notifyTime": 1712600000000
}
```

Fields:

- `sessionId`: tmux session id like `$1`
- `paneId`: tmux pane id like `%3`
- `paneDisplayText`: short label shown in the sidebar
- `notifyTime`: unix time in milliseconds

### `vmux clear`

Append a `clear` event for the same pane identity.

```bash
vmux clear payload.json
# or
vmux clear --payload-path payload.json
```

Payload format:

```json
{
  "sessionId": "$1",
  "paneId": "%3"
}
```

## Finding the right tmux ids

### From inside a tmux pane

If your script is already running inside the pane it wants to mark, this is the easiest setup:

```bash
session_id="$(tmux display-message -p '#{session_id}')"
pane_id="${TMUX_PANE:?TMUX_PANE must be set inside tmux}"
```

### From outside tmux

If you are running from cron, a launch agent, or some external watcher, query the socket directly:

```bash
tmux -L my-socket list-panes -a -F '#{session_name}\t#{session_id}\t#{pane_id}\t#{pane_title}'
```

That gives you the stable ids you need for `notify` and `clear`.

## Strong example 1: an agent running inside tmux asks for attention

This example is for a coding agent or long-running script already running **inside a tmux pane**.
When it hits a state that needs a human, it marks its own pane in `vmux`. When the human has responded or the issue is resolved, it clears the marker.

### Helper script

Save as `~/bin/vmux-attention`:

```bash
#!/usr/bin/env bash
set -euo pipefail

message="${1:?usage: vmux-attention <message>}"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

session_id="$(tmux display-message -p '#{session_id}')"
pane_id="${TMUX_PANE:?must run inside tmux}"
now_ms="$(python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
)"

cat > "$tmp" <<JSON
{
  "sessionId": "$session_id",
  "paneId": "$pane_id",
  "paneDisplayText": "$message",
  "notifyTime": $now_ms
}
JSON

vmux notify "$tmp"
```

And a matching clear helper `~/bin/vmux-attention-clear`:

```bash
#!/usr/bin/env bash
set -euo pipefail

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

session_id="$(tmux display-message -p '#{session_id}')"
pane_id="${TMUX_PANE:?must run inside tmux}"

cat > "$tmp" <<JSON
{
  "sessionId": "$session_id",
  "paneId": "$pane_id"
}
JSON

vmux clear "$tmp"
```

### How an agent would use it

Examples:

```bash
vmux-attention "review requested"
vmux-attention "tests failed"
vmux-attention "deploy needs approval"
```

Later:

```bash
vmux-attention-clear
```

### Why this is strong

This pattern is great when:

- the process already lives inside tmux
- the pane itself is the unit of work
- you want to jump straight back to the exact pane that needs attention

The human workflow becomes:

1. open `vmux`
2. see the recent activity label in the sidebar
3. select that pane
4. land exactly where the agent is waiting

## Strong example 2: a cron job marks a dedicated ops pane from outside tmux

This example is the opposite setup:

- the producer is **not** running inside tmux
- it runs from cron / launchd / CI / a watchdog loop
- it targets a dedicated pane in an `ops` tmux server
- `notify` means “this service needs attention now”
- `clear` means “the service is healthy again”

Assume you keep an ops server on socket `ops` and a pane titled `api-prod`.

### Script

Save as `~/bin/check-api-prod`:

```bash
#!/usr/bin/env bash
set -euo pipefail

socket="ops"
log_path="$HOME/.cache/vmux/ops-recent-activity.jsonl"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

pane_line="$(tmux -L "$socket" list-panes -a \
  -F '#{session_name}\t#{session_id}\t#{pane_id}\t#{pane_title}' \
  | awk -F '\t' '$1 == "ops" && $4 == "api-prod" { print; exit }')"

[ -n "$pane_line" ] || {
  echo "could not find ops/api-prod pane" >&2
  exit 1
}

session_id="$(printf '%s' "$pane_line" | cut -f2)"
pane_id="$(printf '%s' "$pane_line" | cut -f3)"

if curl -fsS https://api.example.com/health >/dev/null; then
  cat > "$tmp" <<JSON
{
  "sessionId": "$session_id",
  "paneId": "$pane_id"
}
JSON
  VMUX_RECENT_ACTIVITY_LOG_PATH="$log_path" vmux clear "$tmp"
else
  now_ms="$(python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
)"
  cat > "$tmp" <<JSON
{
  "sessionId": "$session_id",
  "paneId": "$pane_id",
  "paneDisplayText": "api-prod unhealthy",
  "notifyTime": $now_ms
}
JSON
  VMUX_RECENT_ACTIVITY_LOG_PATH="$log_path" vmux notify "$tmp"
fi
```

### Cron entry

```cron
*/2 * * * * /Users/cole/bin/check-api-prod
```

### How to view it

Open `vmux` against the same tmux socket and log path:

```bash
VMUX_TMUX_SOCKET=ops \
VMUX_RECENT_ACTIVITY_LOG_PATH="$HOME/.cache/vmux/ops-recent-activity.jsonl" \
vmux
```

### Why this is strong

This pattern is great when:

- you have a dedicated operational tmux workspace
- alerts should map to a durable pane, not a chat message
- “resolved” should remove the alert cleanly via `clear`

It turns `vmux` into a lightweight terminal-native incident board.

## Design notes that matter when automating vmux

- `notify` and `clear` are the only persisted event types.
- Identity is always `(session_id, pane_id)`.
- Sidebar state is rebuilt by replaying the event log.
- Malformed lines are ignored line-by-line instead of poisoning the whole log.
- Recent activity expires after roughly **120 seconds** if it is not refreshed.

That last point matters: `vmux` is best for **fresh attention cues**, not permanent issue tracking.
If something should stay visible, your automation should periodically re-emit `notify` until the condition is resolved.

## Practical patterns

A few useful patterns emerge from the subcommands:

- **edge-triggered alert**: send one `notify` when something first fails
- **level-triggered alert**: keep sending `notify` while the condition remains true
- **resolved state**: send `clear` when the pane no longer needs attention
- **per-socket dashboards**: use separate `VMUX_RECENT_ACTIVITY_LOG_PATH` values for separate tmux servers
- **per-agent ownership**: have each agent notify only for its own pane

## When vmux fits best

`vmux` works especially well when:

- tmux is already your workspace shell
- you want terminal-native attention routing
- the right destination is a pane, not a URL or a ticket
- you want simple scripts instead of a larger notification system

It is less appropriate when you need:

- long-term durable alerts
- rich routing rules
- cross-user delivery
- acknowledgement workflows

In those cases, `vmux` is better as the **last-hop pane locator** than the entire alerting system.
