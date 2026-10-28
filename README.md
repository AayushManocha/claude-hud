# claude-hud

HUD for running many Claude Code agents in tmux or [Rex](https://www.superlogical.com/rex/docs): worktrees, chats, status, approvals.

## Setup

```sh
cargo build --release
cc-hud install --write        # adds the hooks to ~/.claude/settings.json (backup: .bak)
```

In `~/.tmux.conf`:

```tmux
set -g @claude-hud-root '~/Developer'      # scanned for repos (2 levels deep)
run-shell ~/Developer/claude-hud/claude-hud.tmux
```

Already have a tmux config? Point your agent at [TMUX_SETUP.md](TMUX_SETUP.md) and it will wire this in without clobbering anything.

Start a workspace with `cc-hud open` (attach-or-create session `dev` rooted at the root dir).
Restart running Claude sessions so they pick up the hooks.

`install` also routes Claude Code's status line through `cc-hud statusline`, which records your plan usage limits (5h / 7d, Pro and Max) for the dashboard header. Your existing status line command keeps running behind it unchanged. Limits update after each Claude response, and show "(Nm old)" when no session has reported for a while.

## Rex

cc-hud also drives Rex terminals through the `rex` CLI. Hooks, the dashboard (list, peek, allow/deny, reply, new worktree/chat, remove), `list`, `next` and `open` work the same; panes are shown as `rex:<id>`.

- The dashboard can run as a tab that stays alive: `cc-hud dashboard --persistent`. `q`/`esc` (or the key again) switches back to the tab you came from, and it pauses its polling while hidden. Jumping to an agent (or creating a worktree or chat) closes it instead, so no dashboard tab is left behind in the session you left; the next `hud` creates a new one. A Rex action in `init.lua` creates the tab (labelled `cc-hud`, one per session) on first use and switches to and from it afterwards; the dashboard runs `rex do hud` (override the action name with `CC_HUD_REX_ACTION`) to switch back, then asks the app to show that tab, which needs Remote Control turned on in Rex's settings (a key press doesn't). Keys in `~/.config/rex/init.lua`: `cmd+shift+h` dashboard, `cmd+shift+n` jump to the agent that has waited longest. `m` in the dashboard toggles the tiled view. Without `--persistent` the dashboard exits on `q` as under tmux, e.g. `rex window new Dashboard -- cc-hud dashboard`.
- The multiplexer for new sessions, settings and notifications is chosen as: `CC_HUD_MUX=tmux|rex`, else the one cc-hud runs inside (`$TMUX`, then `$REX_SESSION`), else tmux if a tmux server is up, else Rex. Existing panes are always reached through their own multiplexer, so both can be in use at once.
- Rex has no options store, status bar or message line: set `CC_HUD_ROOT`, `CC_HUD_CLAUDE_CMD`, `CC_HUD_NOTIFY` and `CC_HUD_PROJECTS` as environment variables, expect no window glyphs or `cc-hud status` bar, and notifications fall back to macOS system notifications. Session tags are kept in `~/.local/state/cc-hud/tags`.
- The dashboard doesn't start on the agent you came from (Rex doesn't report it).

## Keys (after the tmux prefix)

| key | |
|---|---|
| `C` | dashboard popup |
| `M` | dashboard in tiled mission-control view |
| `N` | jump to the agent that has waited longest for you |

Dashboard: `enter`/`space` jump/open, `a` allow once, `d` deny, `r` reply, `w` new worktree, `x` remove worktree / kill chat (confirms, offers force if dirty; keeps the branch),
`c` new chat, `+` add an agent beside the selected one (or in the selected worktree), `R` set the selected agent's role,
`P` pin repos (fuzzy picker), `m` list/grid, `/` filter, `tab` fold, `p` peek, `n` next waiting, `q` quit.

Several agents in one worktree (or chat) are shown under a header row with each agent's status, numbered `#1`, `#2`… in the order they started, with an optional role (`#2 tests`).
The header marks files more than one of them has edited with `⚡`, and the peek lists the files each one edited. `⏎`/`a`/`d`/`r` on the header act on the agent that most needs you; `x` on an agent under it closes just that pane, while `x` on the header removes the worktree or chat as before. `+` splits a pane off the selected agent and starts `claude` in it.

Mouse: click a row (or grid tile) to select it, double-click to jump, click a repo header to fold it, scroll wheel to move (over the preview, it scrolls back through the agent's output); in the pin picker, click to toggle.

## CLI

`cc-hud list|status|next|sync|repos|open|statusline`, `cc-hud new worktree <repo> <branch>`,
`cc-hud new chat <topic> [--cwd DIR]`.

## tmux options

`@claude-hud-root`, `-projects` (comma-separated repo names always shown; other repos appear only while they have an agent), `-next-key`, `-dashboard-key`, `-grid-key`, `-status`, `-window-glyphs`,
`-notify` (off|tmux|system|both), `-claude-cmd`, `-bin`.
Worktrees are created at `<repo parent>/<repo>-wt/<branch>`.
