# claude-hud

tmux HUD for running many Claude Code agents: worktrees, chats, status, approvals.

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

## Keys (after the tmux prefix)

| key | |
|---|---|
| `C` | dashboard popup |
| `M` | dashboard in tiled mission-control view |
| `N` | jump to the agent that has waited longest for you |

Dashboard: `enter`/`space` jump/open, `a` allow once, `d` deny, `r` reply, `w` new worktree,
`c` new chat, `P` pin repos (fuzzy picker), `m` list/grid, `/` filter, `tab` fold, `p` peek, `n` next waiting, `q` quit.

## CLI

`cc-hud list|status|next|sync|repos|open`, `cc-hud new worktree <repo> <branch>`,
`cc-hud new chat <topic> [--cwd DIR]`.

## tmux options

`@claude-hud-root`, `-projects` (comma-separated repo names always shown; other repos appear only while they have an agent), `-next-key`, `-dashboard-key`, `-grid-key`, `-status`, `-window-glyphs`,
`-notify` (off|tmux|system|both), `-claude-cmd`, `-bin`.
Worktrees are created at `<repo parent>/<repo>-wt/<branch>`.
