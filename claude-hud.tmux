#!/usr/bin/env bash
# TPM entrypoint: set -g @plugin 'you/claude-hud'
# Options (set before loading):
#   @claude-hud-bin            path to cc-hud (default: PATH, then ./target/release)
#   @claude-hud-next-key       key (after prefix) to jump to next needs-input agent (default N)
#   @claude-hud-dashboard-key  key (after prefix) to open the dashboard popup (default C)
#   @claude-hud-grid-key       key (after prefix) to open the dashboard in tiled mission-control view (default M)
#   @claude-hud-root           directory scanned for repos (default ~/Developer)
#   @claude-hud-notify         off|tmux|system|both  alert when an agent needs input (default tmux)
#   @claude-hud-claude-cmd     command started in new windows (default claude)
#   @claude-hud-status         on|off  append agent counts to status-right (default on)
#   @claude-hud-window-glyphs  on|off  prefix window names with a status glyph (default on)
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

opt() { local v; v="$(tmux show-option -gqv "$1")"; echo "${v:-$2}"; }

BIN="$(opt @claude-hud-bin "")"
if [ -z "$BIN" ]; then
  if command -v cc-hud >/dev/null 2>&1; then BIN="$(command -v cc-hud)"
  elif [ -x "$DIR/target/release/cc-hud" ]; then BIN="$DIR/target/release/cc-hud"
  elif [ -x "$DIR/target/debug/cc-hud" ]; then BIN="$DIR/target/debug/cc-hud"
  else tmux display-message "claude-hud: cc-hud binary not found (cargo build --release)"; exit 0; fi
fi

# display-popup does not expand formats in its command, so go through run-shell
# (which does) to hand the dashboard the real client name for switch-client.
tmux bind-key "$(opt @claude-hud-dashboard-key C)" run-shell "tmux display-popup -c '#{client_name}' -E -w 98% -h 95% -T ' cc-hud ' \"'$BIN' dashboard --client '#{client_name}'\""
tmux bind-key "$(opt @claude-hud-grid-key M)" run-shell "tmux display-popup -c '#{client_name}' -E -w 100% -h 100% \"'$BIN' dashboard --grid --client '#{client_name}'\""
tmux bind-key "$(opt @claude-hud-next-key N)" run-shell "'$BIN' next --from '#{pane_id}'"

# Idempotent: skip if already wired in (plugin reloads on `source-file`).
if [ "$(opt @claude-hud-status on)" = on ]; then
  cur="$(tmux show-option -gv status-right)"
  case "$cur" in *"cc-hud"*|*"$BIN status"*) ;; *)
    tmux set-option -g status-right "#('$BIN' status --color) $cur" ;;
  esac
fi

if [ "$(opt @claude-hud-window-glyphs on)" = on ]; then
  for o in window-status-format window-status-current-format; do
    cur="$(tmux show-option -gv "$o")"
    case "$cur" in *@hud_glyph*) ;; *)
      tmux set-option -g "$o" "#{?@hud_glyph,#{@hud_glyph} ,}$cur" ;;
    esac
  done
fi
