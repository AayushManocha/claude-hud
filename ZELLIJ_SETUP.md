# Zellij setup

cc-hud picks its backend from the environment: `TMUX` set → tmux, else `ZELLIJ` set → zellij.
Force one with `CC_HUD_MUX=tmux|zellij`.

## How it differs from tmux

- **One session, a tab per worktree/chat.** Zellij's CLI can't move a client between sessions, so
  new workspaces become tabs in the session you're in (or `CC_HUD_SESSION`, default `dev`). Creating
  a tab needs an attached client, so start with `cc-hud open` (runs `zellij attach --create dev`).
- **Jumping works within the current session only.** Agents in other sessions show up in the
  dashboard, but selecting one can't switch you over.
- **Settings are env vars** (`CC_HUD_ROOT`, `CC_HUD_CLAUDE_CMD`, `CC_HUD_NOTIFY`, `CC_HUD_PROJECTS`);
  there are no `@claude-hud-*` options.
- **No status bar counts or tab glyphs.** Zellij can't run a shell command in its bar.
- **Alerts** are system notifications (zellij can't flash a message), unless `CC_HUD_NOTIFY=off`.
- **Tags** (`kind`, `topic`, worktree path) are stored in `<state dir>/zellij-tags/`.

## Keybinds

Add inside the `tmux { … }` keybind block (the prefix mode) of `~/.config/zellij/config.kdl`:

```kdl
bind "a" {
    Run "/path/to/cc-hud" "dashboard" { floating true; close_on_exit true; x "1%"; y "2%"; width "98%"; height "95%"; }
    SwitchToMode "normal"
}
bind "M" {
    Run "/path/to/cc-hud" "dashboard" "--grid" { floating true; close_on_exit true; x "0%"; y "0%"; width "100%"; height "100%"; }
    SwitchToMode "normal"
}
bind "N" {
    Run "/path/to/cc-hud" "next" { floating true; close_on_exit true; x "40%"; y "45%"; width "20%"; height "10%"; }
    SwitchToMode "normal"
}
```

Then `cc-hud install --write` for the Claude Code hooks, as with tmux.
