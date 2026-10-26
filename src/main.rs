mod ansi;
mod config;
mod dashboard;
mod discover;
mod event;
mod git;
mod install;
mod interact;
mod lifecycle;
mod list;
mod model;
mod mux;
mod next;
mod notify;
mod status;
mod sync;
mod state;
mod transcript;
mod usage;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cc-hud", about = "tmux/zellij HUD for Claude Code agents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum New {
    /// New git worktree for `repo` on `branch` (created if missing), with a claude window
    Worktree {
        repo: String,
        branch: String,
        /// Don't start claude in the new window
        #[arg(long)]
        no_claude: bool,
    },
    /// New chat window (not tied to a worktree)
    Chat {
        topic: String,
        /// Working directory (default: the root directory)
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Claude Code hook entrypoint (reads hook JSON on stdin)
    Event {
        /// session-start | user-prompt-submit | pre-tool-use | post-tool-use | permission-request | notification | stop | session-end
        hook: String,
    },
    /// List known agents
    List {
        #[arg(long)]
        json: bool,
    },
    /// One-line agent counts for the tmux status bar (also refreshes window glyphs)
    Status {
        /// Emit #[fg=...] tmux colour codes
        #[arg(long)]
        color: bool,
    },
    /// Jump to the next agent that needs input
    Next {
        /// Pane the key was pressed in (pass #{pane_id}) so repeated presses cycle
        #[arg(long)]
        from: Option<String>,
    },
    /// Full-screen agent dashboard (run inside `tmux display-popup`)
    Dashboard {
        /// tmux client to switch when jumping (pass #{client_name})
        #[arg(long)]
        client: Option<String>,
        /// Start in mission-control (tiled) view
        #[arg(long)]
        grid: bool,
    },
    /// List repos and worktrees found under the root directory
    Repos {
        #[arg(long)]
        json: bool,
    },
    /// Create a worktree or chat window
    New {
        #[command(subcommand)]
        what: New,
    },
    /// Attach to (or create) the workspace tmux session rooted at the root directory
    Open,
    /// Claude Code status line command: records plan usage limits for the dashboard,
    /// then runs --then (your previous status line) with the same input
    Statusline {
        #[arg(long)]
        then: Option<String>,
    },
    /// Refresh per-window status glyphs now
    Sync,
    /// Add the hooks to Claude Code settings (dry run unless --write)
    Install {
        #[arg(long)]
        write: bool,
        /// Settings file (default ~/.claude/settings.json)
        #[arg(long)]
        settings: Option<std::path::PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Event { hook } => {
            event::run(&hook);
            Ok(())
        }
        Cmd::List { json } => list::run(json),
        Cmd::Status { color } => {
            status::run(color);
            Ok(())
        }
        Cmd::Next { from } => next::run(from),
        Cmd::Dashboard { client, grid } => dashboard::run(client, grid),
        Cmd::Repos { json } => {
            let repos = discover::scan(&config::root());
            if json {
                println!("{}", serde_json::to_string_pretty(&repos)?);
            }
            for r in &repos {
                if !json {
                    println!("{}  {}", r.name, r.path.display());
                    for w in &r.worktrees {
                        println!("    {:<24} {}", w.branch.as_deref().unwrap_or("(detached)"), w.path.display());
                    }
                }
            }
            Ok(())
        }
        Cmd::New { what } => {
            let msg = match what {
                New::Worktree { repo, branch, no_claude } => {
                    let repos = discover::scan(&config::root());
                    let r = repos
                        .iter()
                        .find(|r| r.name == repo)
                        .ok_or_else(|| anyhow::anyhow!("no repo named {repo} under {}", config::root().display()))?;
                    lifecycle::new_worktree(None, r, &branch, !no_claude)?
                }
                New::Chat { topic, cwd } => lifecycle::new_chat(None, &topic, &cwd.unwrap_or_else(config::root))?,
            };
            println!("{msg}");
            Ok(())
        }
        Cmd::Open => {
            let session = std::env::var("CC_HUD_SESSION").unwrap_or_else(|_| "dev".into());
            Err(mux::get().attach_workspace(&session, &config::root()).into())
        }
        Cmd::Sync => {
            sync::run();
            Ok(())
        }
        Cmd::Statusline { then } => usage::statusline(then),
        Cmd::Install { write, settings } => install::run(write, settings),
    }
}
