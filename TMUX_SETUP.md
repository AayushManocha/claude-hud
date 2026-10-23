# Configuring tmux for claude-hud (agent guide)

You are an agent helping a user wire claude-hud into their **existing** tmux config. Goal: the
smallest set of changes that makes `cc-hud` work well, without disturbing their setup.

Rules of engagement:

- Read their config first. Never overwrite or restructure it. Back it up (`cp ~/.tmux.conf ~/.tmux.conf.bak`) before the first edit and make additive, commented edits.
- Show the user the diff and get a yes before writing. Don't reload their tmux server without saying so (`source-file` is safe; killing the server is not).
- Prefer asking about a conflict over guessing.

## 1. Discover their setup

Run these and read the output before proposing anything:

```sh
tmux -V                                   # need >= 3.2 for display-popup (the dashboard)
command -v cc-hud; ls <repo>/target/release/cc-hud   # binary built? (cargo build --release)
ls -la ~/.tmux.conf ~/.config/tmux/tmux.conf 2>/dev/null   # which config file is live
tmux show -g status-right status-right-length status-interval window-status-format window-status-current-format
tmux list-keys | grep -E 'prefix +(C|M|N) '   # conflicts with the default HUD keys
tmux show -gv default-terminal; tmux show -g terminal-overrides terminal-features
grep -nE 'tpm|@plugin|catppuccin|dracula|tokyo|nord|powerline|status-(left|right|format)|window-status|bind +-?[nT]* *[CMN]\b|detach-on-destroy|automatic-rename' <their tmux.conf>
```

Also follow any `source-file` lines to find the real definitions.

## 2. What claude-hud does to tmux (so you can reason about conflicts)

`claude-hud.tmux` (run via `run-shell`, or TPM) does the following at load time:

- **Binds three keys** after the prefix: `C` dashboard popup, `M` tiled mission-control popup, `N` jump to the agent that has waited longest. Override with `@claude-hud-dashboard-key`, `-grid-key`, `-next-key`.
- **Prepends** `#(cc-hud status --color)` to the *current* `status-right` (once; guarded by a string match on `cc-hud`).
- **Prepends** `#{?@hud_glyph,#{@hud_glyph} ,}` to the *current* `window-status-format` and `window-status-current-format` (once; guarded on `@hud_glyph`).
- Reads its own settings from `@claude-hud-*` global options at runtime and from `@hud_*` session/window options it sets itself.

Agents live in **their own tmux sessions** (one session per worktree or chat, created detached, with `automatic-rename off` on the window). Jumping uses `switch-client`. The status glyph is the window option `@hud_glyph`, refreshed whenever tmux runs `#(cc-hud status)`, i.e. every `status-interval`.

Consequences, which drive everything below:

1. Because it *prepends to whatever is already there*, **load order matters**.
2. Because status refresh is driven by `status-interval`, a slow interval means stale glyphs/counts.
3. Because every agent is a session, session-navigation settings matter.
4. Themes that own `status-right` / `window-status-*` can clobber or bypass the injection.

## 3. Changes to make

### 3.1 Add the plugin (required)

Add to the config, **after** every line that sets `status-right`, `window-status-format` and `window-status-current-format` (including theme plugins), and with options **before** the `run-shell`:

```tmux
# --- claude-hud ---
set -g @claude-hud-root '~/Developer'        # ask the user: where do their repos live?
# set -g @claude-hud-projects 'repo-a,repo-b' # optional: always-shown repos
run-shell <path-to-claude-hud>/claude-hud.tmux
```

- With TPM: `set -g @plugin '<owner>/claude-hud'` goes with the other plugins, and it must sit **above** `run '~/.tmux/plugins/tpm/tpm'`, which should stay the last line. Theme plugins must be listed *before* claude-hud, since TPM loads in order.
- If `cc-hud` is not on `PATH` and not in `target/release`, set `@claude-hud-bin` to its absolute path.
- Also run `cc-hud install --write` once (hooks into `~/.claude/settings.json`; it writes a `.bak`). Mention that already-running Claude sessions must be restarted.

### 3.2 Status refresh (strongly recommended)

```tmux
set -g status-interval 5
```

Default is 15s. Don't go below 2–3: each tick spawns `cc-hud status`. If they already have a value `<= 10`, leave it.

### 3.3 Status bar width (recommended)

`status-right-length` defaults to 40 and silently truncates. The HUD counts are prepended, so *their existing right side* gets cut off. If they use a long `status-right`, raise it:

```tmux
set -g status-right-length 120
```

### 3.4 Colours in peek and grid previews (recommended)

Previews use `capture-pane -e` and render the pane's real colours. Make sure the outer terminal advertises truecolor, otherwise previews look washed out:

```tmux
set -g default-terminal "tmux-256color"
set -as terminal-features ",*:RGB"        # tmux >= 3.2; older: set -ga terminal-overrides ",*:Tc"
```

Only add if `default-terminal` / RGB aren't already configured.

### 3.5 Session navigation (recommended, since agents are sessions)

```tmux
set -g detach-on-destroy off    # closing an agent session moves you to another one instead of detaching
```

Check their status line shows the session name (`#S`) somewhere, or they'll lose track of which agent they're in. Don't add it if their theme already does. If they have no session-switching binding, mention `prefix + s` (built-in choose-tree) and `prefix + (` / `)`.

### 3.6 Notifications

`@claude-hud-notify` is `tmux` (default; a `display-message` for 5s), `system`, `both` or `off`. If the user finds the 5s message too brief, offer `set -g display-time 5000`; that also affects their other messages, so ask first.

## 4. Conflicts and special cases

- **Key conflicts.** Defaults `C`, `M`, `N` are not free in every config: in tmux ≥ 3.2 `prefix C` is `customize-mode`, `prefix M` is `select-pane -M`. claude-hud rebinds them, silently shadowing the built-ins. If the user relies on those or already `bind`s them (check the `list-keys` output from step 1), pick alternatives and set them *before* `run-shell`, e.g. `set -g @claude-hud-dashboard-key g`. Never remove their existing binds.
- **Themes (catppuccin, dracula, tokyo-night, powerline, etc.).**
  - If the theme builds `status-right` from its own variables/modules, plugin order decides whether the HUD segment survives. Load the theme first. After `source-file`, verify with `tmux show -g status-right`.
  - If the theme sets `window-status-format` via a plugin option and re-applies it later (or on `source-file`), the glyph prefix can vanish. Fix by turning off auto-injection and including the glyph in the theme's format yourself: `set -g @claude-hud-window-glyphs off`, then add `#{?@hud_glyph,#{@hud_glyph} ,}` to the start of the format the theme exposes.
  - If they'd rather own the status-right placement: `set -g @claude-hud-status off`, then put `#('<path>/cc-hud' status --color)` exactly where they want it.
- **Status bar disabled / `status off` / top bar only.** The glyphs and counts won't show; the dashboard (`prefix C`) and `prefix N` still work.
- **Custom `prefix`, `prefix2`, or key tables.** Keys are bound in the `prefix` table, so nothing to change. If they use a custom root-table style (e.g. `bind -n`), ask whether they want the HUD keys there too; add with `bind -n <key> run-shell ...` copying the lines in `claude-hud.tmux`, not by guessing.
- **Reloading.** `source-file` is safe: both injections are idempotent. If glyphs or status-right look doubled after reload, the guard string was defeated by a theme rewriting the value. Reset the format and reload, don't add more edits.
- **Old tmux (< 3.2).** No `display-popup`; the dashboard keys won't work. Tell the user to upgrade rather than working around it.
- **Nested tmux / SSH.** The popup runs in the *client* tmux. The HUD must run in the same tmux server as the agents. If the user works over SSH, install and configure on the remote host.
- **`automatic-rename` / `allow-rename` globally on.** Not a problem. The HUD turns off `automatic-rename` on windows it creates. Don't change their global value.

## 5. Verify

After the edits (and with the user's OK to reload):

```sh
tmux source-file <their tmux.conf>
tmux list-keys | grep -E 'cc-hud'                  # three bindings present
tmux show -g status-right                          # starts with #('...cc-hud' status --color)
tmux show -g window-status-format                  # starts with #{?@hud_glyph,...}
tmux show -g status-interval
cc-hud status --color; echo "exit=$?"              # runs without error (empty output = no agents, that's fine)
cc-hud list                                        # lists agents, or empty
```

Then ask the user to try `prefix C` (dashboard popup should open, `q` to close). If a key does nothing, `tmux display-message` output or `tmux show-messages` usually names the cause (binary not found, or an old tmux).

## 6. Report back

Summarise for the user: what lines you added and where, anything you intentionally left alone (and why), key choices they might want to change, and the one-time steps they still must do themselves (build the binary, `cc-hud install --write`, restart running Claude sessions).
