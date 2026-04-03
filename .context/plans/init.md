---
goal: vmux-vertical-tabs
---

# vmux: Vertical tmux Sessions

## Goal
**Goal document:** [vmux-vertical-tabs](../goals/vmux-vertical-tabs.md)

Create a small wrapper experience around tmux whose primary job is to make tmux sessions feel like a **vertical tab sidebar**, with the main entry point being a single `vmux` command with no subcommands.

The product should focus on one thing only: **showing tmux sessions vertically** while tmux continues to own the real session and window state.

## Motivation
The built-in tmux status bar is horizontal, and that shape does not match the workflow the user wants. The current tmux setup already has custom color behavior, bell highlighting, cwd-aware splits, and window management conventions, which suggests the UI problem is not lack of state — it is presentation.

A vertical sidebar-style view would make the active session list easier to scan, preserve the sense of a persistent workspace rail, and better match the user's mental model of “tabs” than tmux's default bottom bar.

## Ideal End State
A user can run `vmux` and immediately get a terminal experience where tmux sessions are represented vertically instead of horizontally.

In the ideal end state:

- tmux remains the source of truth for sessions, windows, panes, bell state, and titles
- the native tmux status bar is hidden
- the vertical sidebar stays synchronized with the live tmux state
- active, inactive, and alerting sessions are visually distinct
- the experience feels like a purpose-built vertical session UI rather than a hacked-around status line
- the project stays narrowly scoped and does not expand into a general tmux replacement

## Table of Contents
1. Scope and non-goals
2. Command behavior
3. Experience target
4. State, sync, and fidelity
5. Implementation latitude
6. Verification and system tests
7. Open questions
8. Rough phases

## Scope and non-goals
### In scope
- A wrapper command surface centered on a single `vmux` command with no subcommands
- A vertical representation of tmux sessions
- Sessions as the primary visible and testable unit; windows and panes may appear only as subordinate details
- Sync with tmux state that the user already cares about, including active session and bell-driven attention cues
- Respect for the existing tmux workflow, including cwd-aware pane/window behavior inside each session

### Out of scope
- Replacing tmux as a terminal multiplexer
- Adding unrelated terminal UI features
- Inventing a new session model that diverges from tmux
- Broad customization beyond what is needed to make the vertical session view feel good

## Ownership summary
Tmux owns session lifecycle, panes/windows, process lifetimes, and configuration. Vmux owns the vertical session view, the single `vmux` entry point, and any UI-only state needed to render the sidebar. Vmux should not become a second owner of durable tmux metadata or write tmux configuration files behind the user’s back.

Vmux may keep only ephemeral UI state in memory, such as focus, selection, layout geometry, and attention fade timers. It must not persist a second session registry, labels, or workspace metadata that tmux cannot reconstruct.

## Command behavior
`vmux` should feel like a single entry point into the vertical-session experience, not a shell for separate modes or subcommands.

At a high level, the command should:

- open the vertical session UI
- let the user choose or create the session they want
- keep the interaction centered on session switching and workspace entry
- avoid exposing a larger command surface unless it is truly needed for the core experience

The interaction should be interactive-first: the user should not need subcommands to reach the intended flow, and the plan does not assume a broad flag surface for v1. If flags are ever added, they should remain secondary and not turn into a separate mode system.

Preferred v1 hosting model: a standalone companion process that owns the terminal while the session chooser is active and talks to tmux as a client, rather than living inside a tmux pane. That keeps the UI focused on the session rail and avoids making tmux’s layout the host layout.

By default, `vmux` opens a session chooser. The chooser lets the user pick an existing tmux session or create a new real tmux session through tmux itself. If tmux is unavailable, vmux should fail clearly instead of inventing a fallback mode.

## Experience target
The UI should make the session list feel like a sidebar, not like a secondary dashboard.

That means the design should prioritize:

- fast visual scanning of sessions
- clear active-session indication
- obvious attention cues when a session has a bell or other noteworthy state
- a layout that can be comfortably used day-to-day, not just demonstrated once

The best version of this project should feel boring in the right way: it should disappear into the workflow and simply make tmux easier to read.

## State, sync, and fidelity
The main architectural principle is simple: **tmux owns truth; vmux renders it**.

That means vmux may derive transient UI-only state, like attention fade timing, but it should not invent parallel durable session state.

Vmux must tolerate external tmux changes made outside vmux and reconcile against live tmux state on every run.

The UI layer must stay aligned with tmux changes such as:

- session selection changes
- session creation and deletion
- renames/titles
- bell state and any time-based fading behavior
- attach/detach transitions
- live layout changes that affect what the user sees

Because some of the current visual cues are time-dependent rather than purely event-driven, the plan should assume that the vertical UI will need both event-triggered refreshes and periodic redraws. The exact sync strategy is still open, but it should be chosen early enough to support the prototype without re-framing the whole project.

## Implementation latitude
The implementation language is intentionally open.

The choice should be driven by whichever stack can most cleanly support:

- tmux process integration
- a responsive vertical text UI
- reliable refresh behavior
- easy packaging for the `vmux` entry point

The project should optimize for clarity and durability rather than picking a language for its own sake.

## Verification and system tests
The default test strategy should be system tests that prove real user-visible behavior. The tests should run the actual `vmux` binary, keep tmux and the filesystem real, and substitute only the hosting substrate needed for isolation.

The preferred seam is a `vmux` process running against an isolated tmux server and a temporary terminal environment. That lets tests verify outcomes such as:

- the sidebar reflects the real tmux session list
- selecting a session attaches to that session
- creating a session through `vmux` creates a real tmux session
- bell or attention cues appear when tmux emits them
- refreshes and resizes keep the sidebar readable and aligned
- `vmux` fails clearly when tmux is unavailable or misconfigured

Minimal v1 system-test battery:

1. list and select an existing session
2. create a session via `vmux` and prove it exists in tmux
3. emit a bell in a non-active session and observe a visible attention cue
4. resize the terminal and confirm the sidebar remains readable
5. run without tmux available and confirm a clear non-destructive failure

These tests should use a temporary tmux socket, temp home/config, and a pty or terminal harness. They should assert durable or visible outcomes: tmux session state, chosen session, rendered text or layout, and exit behavior. Wherever session existence or selection matters, the test should also verify the tmux oracle directly (for example via `tmux list-sessions` / `tmux list-windows`) rather than trusting only vmux-rendered text. They should not mock tmux, replace persistence with fakes, or prove only that helper methods were called.

## Open questions
The host model and chooser-first default are fixed decisions in this plan. The remaining questions below are only about presentation tuning within that choice.

A few things still need to be settled before the plan becomes concrete:

- How much width is acceptable to reserve for the sidebar in the default experience?
- Should the sidebar be always visible, or should it be collapsible?
- How faithfully should attention coloring match the current tmux status behavior?

## Rough phases
1. **Experience framing** — document the chosen hosting model, the chooser-first `vmux` behavior, and the minimum session data the vertical view must show. Phase 1 should end with those choices stated plainly so technical locking can proceed without reinterpreting the plan.
2. **State mapping** — lock how tmux data is projected into the vertical UI and choose the initial sync strategy, using the Phase 1 decisions as fixed inputs.
3. **Prototype** — build the simplest version that proves the sidebar feels right, starting with the smallest useful session/attention model, and add the first real system tests alongside it.
4. **Refinement** — tighten the color behavior, live updates, command flow, and system-test coverage without expanding the scope beyond vertical session navigation.

Each phase should end with a short decision or proof artifact before the next phase starts: Phase 1 writes the chosen host/default/visible-field decisions, Phase 2 writes the tmux→vmux state projection and sync choice, Phase 3 demonstrates the vertical UI with the minimal system-test battery, and Phase 4 only adjusts polish and extends coverage. Phase 2 is not done until the sync strategy is explicit enough for the prototype and the system tests to use the same seam.

The project is successful when the user can stop thinking about the horizontal status bar entirely and just use tmux through a vertical session experience.
