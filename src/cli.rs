use crate::commands;
use crate::error::JavelinError;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "javelin", version, disable_version_flag = true)]
#[command(about = "Local, agent-native version control")]
pub struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    pub project: Option<PathBuf>,
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init {
        path: Option<PathBuf>,
    },
    Version,
    Status {
        #[arg(long)]
        ignored: bool,
    },
    Checkpoint {
        #[arg(long)]
        reason: Option<String>,
    },
    Diff {
        from: Option<String>,
        to: Option<String>,
        #[arg(last = true)]
        path: Vec<String>,
    },
    History {
        #[arg(long)]
        layer: Option<String>,
        #[arg(long)]
        path: Option<String>,
    },
    Show {
        reference: String,
    },
    Refresh {
        layer: Option<String>,
    },
    Verify {
        layer: Option<String>,
    },
    Publish {
        layer: Option<String>,
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    Discard {
        layer: Option<String>,
        #[arg(long)]
        cascade: bool,
        #[arg(long)]
        reparent: Option<String>,
        #[arg(long)]
        purge: bool,
    },
    World(WorldArgs),
    Layer(LayerArgs),
    Conflict(ConflictArgs),
    Provenance(ProvenanceArgs),
    Explain {
        path: String,
    },
    Claim(ClaimArgs),
    Events {
        #[arg(long, default_value_t = 0)]
        since: i64,
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        jsonl: bool,
    },
    Hook(HookArgs),
    Doctor,
    Fsck,
    Repair {
        #[arg(long)]
        view: Option<String>,
    },
    Gc {
        #[arg(long)]
        dry_run: bool,
    },
    Discarded(DiscardedArgs),
    #[command(name = "__monitor", hide = true)]
    Monitor,
}

#[derive(Debug, Args)]
pub struct WorldArgs {
    #[command(subcommand)]
    pub command: WorldCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorldCommand {
    Current,
    History,
    Restore {
        version: String,
        #[arg(long)]
        accept_failing: bool,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct LayerArgs {
    #[command(subcommand)]
    pub command: LayerCommand,
}

#[derive(Debug, Subcommand)]
pub enum LayerCommand {
    Create {
        name: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long, default_value = "world")]
        target: String,
        #[arg(long)]
        claim: Vec<String>,
    },
    List,
    Show {
        layer: String,
    },
    Path {
        layer: String,
    },
    Restore {
        checkpoint: String,
        #[arg(long)]
        layer: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct ConflictArgs {
    #[command(subcommand)]
    pub command: ConflictCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConflictCommand {
    List {
        layer: Option<String>,
    },
    Show {
        id: String,
    },
    Resolve {
        id: String,
        #[arg(long, value_parser = ["base", "target", "private", "edited"])]
        r#use: String,
    },
}

#[derive(Debug, Args)]
pub struct ProvenanceArgs {
    #[command(subcommand)]
    pub command: ProvenanceCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProvenanceCommand {
    Begin {
        #[arg(long)]
        layer: Option<String>,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    Event {
        #[arg(long)]
        session: String,
        #[arg(long)]
        event_type: String,
        #[arg(long)]
        payload: Option<String>,
    },
    Attach {
        #[arg(long)]
        session: String,
        path: PathBuf,
        #[arg(long)]
        media_type: Option<String>,
    },
    End {
        session: String,
    },
    Show {
        session: String,
        #[arg(long)]
        raw: bool,
    },
    Search {
        query: String,
    },
    Purge {
        session: String,
    },
}

#[derive(Debug, Args)]
pub struct ClaimArgs {
    #[command(subcommand)]
    pub command: ClaimCommand,
}

#[derive(Debug, Subcommand)]
pub enum ClaimCommand {
    List,
    Renew {
        id: String,
        #[arg(long, default_value_t = 3600)]
        seconds: u64,
    },
    Release {
        id: String,
    },
}

#[derive(Debug, Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub command: HookCommand,
}

#[derive(Debug, Subcommand)]
pub enum HookCommand {
    OperationStart {
        #[arg(long)]
        session: Option<String>,
    },
    OperationEnd {
        #[arg(long)]
        session: Option<String>,
    },
    SessionStart {
        #[arg(long)]
        session: Option<String>,
    },
    SessionEnd {
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct DiscardedArgs {
    #[command(subcommand)]
    pub command: DiscardedCommand,
}

#[derive(Debug, Subcommand)]
pub enum DiscardedCommand {
    List,
    Recover { layer: String },
    Purge { layer: String },
}

pub fn run() -> i32 {
    let cli = Cli::parse();
    let json = cli.json;
    match commands::execute(cli) {
        Ok(()) => 0,
        Err(error) => {
            render_error(&error, json);
            i32::from(error.exit_code)
        }
    }
}

fn render_error(error: &JavelinError, json: bool) {
    if json {
        eprintln!(
            "{}",
            serde_json::to_string(&error.json()).unwrap_or_else(|_| error.message.clone())
        );
    } else {
        eprintln!("error [{}]: {}", error.code, error.message);
        for recovery in &error.recovery {
            eprintln!("recovery: {recovery}");
        }
    }
}
