# Goal: vmux session tabs

**Give tmux a persistent vertical session sidebar so workspaces are easier to scan and switch without changing tmux’s underlying role as the source of truth.**

## Motivators

- The built-in tmux status bar is horizontal, but the desired workflow is easier to read as a vertical rail.
- The current tmux setup already tracks useful state like session activity, bell attention, cwd, and titles; the missing piece is presentation.
- A sidebar-style session view should reduce visual friction and make switching among workspaces feel more direct.
- Without this, the user keeps working around a UI shape that does not match the way they think about sessions.

## In Scope

- The tmux session list as a persistent vertical sidebar.
- A single `vmux` entry point with no subcommands.
- Visibility of the active session and attention-worthy sessions in a vertically arranged view.
- Use of tmux’s existing session/window/pane state as the authoritative data source.
- The user-facing workflow for navigating and switching tmux-backed workspaces through the vertical UI, all through the one `vmux` command.
- Sessions as the primary visible unit, rather than attempting to turn tmux windows or panes into the sidebar’s main abstraction.
- A split-view host where the sidebar stays visible while the chosen session is shown alongside it.

## Out of Scope

- Replacing tmux as a multiplexer.
- General terminal-dashboard features unrelated to the vertical session rail.
- Broader workspace orchestration beyond showing tmux sessions vertically.
- Changing the user’s broader shell or terminal environment outside what is needed for this experience.
- Making tmux windows or panes the primary visible unit in the sidebar.
- Replacing tmux’s terminal behavior with a different multiplexer model.

## References

- `/Users/cole/vmux/.context/plans/init.md`
- `/Users/cole/.tmux.conf`
- `/Users/cole/.zshrc`
