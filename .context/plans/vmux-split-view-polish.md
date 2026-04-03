---
goal: vmux-vertical-tabs
---

# vmux Split-View Polish

## Goal

Refine vmux’s split-view host so the left session rail feels like a true vertical tab sidebar: borderless, mouse-clickable, and able to yield most keyboard input to tmux by default.

**Goal document:** [vmux-vertical-tabs](../goals/vmux-vertical-tabs.md)

## Motivation

The split-view host is now the right structural shape, but the interaction still needs to feel like a real daily driver instead of a prototype shell. Borders add visual noise, and the current input model still asks the user to think about host-vs-pane ownership too often.

This pass is also the right time to make the whole host materially more robust: the terminal should recover cleanly, tmux/session churn should be tolerated, input forwarding should be predictable, and the split view should remain usable under the kinds of resize and lifecycle events that happen in real work.

The user experience we want is closer to “tmux sessions on the left, live session on the right, and the terminal mostly just works” than a modal app with obvious panes and chrome. Mouse clicks should feel natural for choosing tabs, and most keystrokes should go to tmux unless vmux explicitly needs them.

## Ideal End State

A user runs `vmux` and sees a clean borderless split view:

- the left rail lists tmux sessions vertically
- the right side shows the selected tmux session
- clicking a session in the rail immediately selects it
- the default interaction feels like pass-through-most mode, where tmux receives most input
- vmux only intercepts the keys and gestures it truly owns, such as session selection, a small set of host controls, and exit
- the experience stays responsive in debug builds and does not feel heavyweight to use

In the ideal end state, the sidebar is a control surface, not a decorated widget tree.

## Table of Contents

1. Scope and non-goals
2. Semantic model
3. Interaction model
4. Risks and assumptions
5. Checkpoints and guarantees
6. Open questions

---

## Scope and non-goals

### In scope

- Remove borders and heavy chrome from the split view.
- Keep the session rail visually compact and readable without boxed panels.
- Support mouse selection of sessions by clicking in the rail.
- Support a passthrough-most input model where tmux receives the majority of keyboard input.
- Preserve the current split-view ownership model: vmux owns the host terminal, tmux owns the selected session content.
- Keep the app fast enough to feel good in debug builds.

### Out of scope

- Reintroducing a chooser-only handoff flow.
- Adding subcommands or a broader CLI surface.
- Turning vmux into a generic terminal dashboard.
- Adding durable vmux-owned state beyond the selection and rendering state needed for the host UI.
- Changing tmux session lifecycle semantics; vmux may select sessions, but tmux remains the authority for creating, deleting, and renaming them.
- Introducing advanced tab management semantics such as reordering, close-on-middle-click, or context menus in this pass; this polish phase is only about selection, focus, and forwarding behavior.

## Semantic model

The split view has two regions and two kinds of ownership:

- **Session rail**: a vertical list of tmux sessions that vmux owns and renders.
- **Tmux pane**: the live session view that tmux owns and vmux embeds.

The important state machine is input ownership:

- **pass-through-most mode**: keyboard input flows to the tmux pane unless vmux explicitly claims it
- **selection mode**: session-rail navigation and mouse clicks update the selected session
- **host control keys**: a small set of keys remain vmux-owned for quitting, switching focus, or changing the active session

For v1, the concrete baseline is intentionally small: `q` / `Esc` / `Ctrl-q` quit, `Tab` toggles between rail focus and pane focus, and mouse clicks select rows in the rail. While the rail has focus, `j` / `k` / arrow keys move selection; while the pane has focus, those same keys are forwarded to tmux. Quit keys are always vmux-owned, even when the pane has focus.

The key semantic boundary is that tmux remains the source of truth for the session contents, while vmux owns only the host shell around it and the selection state.

Pass-through-most is the default interaction contract: anything not in the reserved vmux-owned set is forwarded unchanged to tmux, and tmux’s own bindings keep their meaning. The initial reserved vmux-owned set should be intentionally tiny: a quit key, a single focus-toggle gesture, and mouse clicks on session rows. Selection mode is transient and only exists while the user is explicitly acting on the session rail or while the rail has keyboard focus.

Mouse interaction is part of that boundary: clicking a session in the rail is a vmux-owned selection action, not a tmux action. Whitespace in the rail is ignored, and the selected session should always be visually distinguished without borders using a concrete treatment such as bold text plus a subtle background shade or left-edge accent.

## Interaction model

The intended interaction is:

1. vmux starts in split view.
2. The currently selected session appears on the right.
3. The rail on the left stays visible at all times.
4. Clicking a session tab selects it and updates the right-hand tmux pane.
5. Most keystrokes go to tmux by default.
6. vmux only intercepts a small, explicit set of host-level controls.
7. The reserved host set should stay tiny: quit plus a single focus-toggle gesture are the baseline, and everything else is forwarded unless explicitly reserved.
8. For v1, selection is keyed by a straightforward click or keyboard action; drag, scroll, and right-click should not change the selected session.
9. Keyboard focus is explicit: when the rail is focused, `j` / `k` / arrow keys move the selection; when the pane is focused, those same keys are forwarded to tmux.
10. Selection is not a hidden mode; it is just the moment vmux is processing rail-focused interaction or a row click.

The rail should feel like tabs, not like a separate application area.

## Risks and assumptions

- tmux may change sessions while vmux is running; vmux must treat tmux as authoritative on refresh. Mitigation: always reconcile the rail against live tmux state before applying selection changes.
- Mouse reporting must be good enough for row clicks to be reliable. Mitigation: keep keyboard selection as the fallback path so the experience remains usable if mouse reporting is degraded.
- The reserved vmux-owned key set must stay small so tmux-forwarding remains the dominant behavior. Mitigation: treat any new host key as an explicit contract change, not an incidental shortcut.
- Debug builds must stay interactive enough that the split view can be dogfooded while iterating. Mitigation: keep the hot path optimized and measure with a simple repeatable smoke scenario.

## Checkpoints and guarantees

### 1. Borderless split view
The first checkpoint guarantees that the host no longer depends on heavy borders or panel chrome for structure. The layout is readable by spacing, highlight state, and alignment alone, and the selected session remains visibly distinguishable at a glance with a concrete non-border indicator (for example: bold text plus a subtle background shade or left-edge accent). No border-drawing characters should appear around the rail or pane.

### 2. Mouse-driven session selection
The second checkpoint guarantees that clicking a rail item changes the selected tmux session and updates the embedded pane. Clicks on whitespace are ignored, and drag/scroll/right-click do not change selection.

### 3. Pass-through-most input mode
The third checkpoint guarantees that the default keyboard path feels tmux-first: vmux only intercepts the keys it owns, and ordinary typing mostly reaches the live session. The initial reserved vmux-owned set is small and explicit, and any key outside that set is forwarded unchanged to tmux. Example expectations: tmux prefix combos, ordinary letters/digits, Enter, Ctrl+C in the tmux pane, and common arrow-key navigation in pane focus all behave as they would in plain tmux unless they are explicitly reserved by vmux. Up/Down/j/k are vmux-owned only while the rail has keyboard focus.

### 4. Usable debug performance
The fourth checkpoint guarantees that the host remains responsive in development builds so the split view is practical to use while iterating. Switching sessions, typing, and clicking should feel immediate enough to dogfood, with a concrete smoke target such as session switches completing in roughly 100ms on a reference machine and no visible input buffering during a short typing burst.

### 5. Lifecycle and failure robustness
The fifth checkpoint guarantees that the host stays clean and predictable across the ugly cases: terminal teardown, tmux session disappearance, embedded client exit, resize churn, and mouse input quirks. The split view should recover or fail loudly rather than getting stuck in a half-live state. A minimal verification set for this checkpoint should include: (a) a selected session disappears from tmux and the rail updates without corruption, (b) the embedded client exits and vmux returns to a clean state instead of freezing, and (c) repeated resizes do not leave stale layout artifacts.

### 6. Real-process verification
The final checkpoint guarantees that the behavior is proven through the real vmux binary and a real isolated tmux-backed integration path, not a fake-only UI test. The verification set should include clicking a session, forwarding text into the pane, switching sessions without losing terminal cleanliness, and observing a rail update when tmux removes or changes a session. The scenarios should cover at least: start vmux with three sessions; switch via mouse click; type a line into the pane; use tmux-prefix-style behavior in the pane; observe tmux-side session changes reflected back in the rail; and observe a clean failure or exit when the embedded client ends.

## Open questions

- Should the rail support mouse hover/visual feedback, or is click-only enough for v1?
- What is the smallest visual treatment that makes the session rail feel like tabs without reintroducing borders?
