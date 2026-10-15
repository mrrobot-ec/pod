use std::env;

use clap::{CommandFactory, Parser, Subcommand, error::ErrorKind};

use crate::clock::SystemClock;
use crate::config::Config;
use crate::engine::Engine;
use crate::error::{PodError, Result};
use crate::herdr::{Herdr, socket};
use crate::identity;
use crate::paths::{self, Paths};
use crate::store::Store;

pub mod commands;
pub mod output;

use commands::{env as env_commands, gates, messages, recovery, runs, tasks, workers};

#[derive(Debug, Parser)]
#[command(
    name = "pod",
    version,
    about = "Supervised multi-agent orchestration for herdr"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true, value_name = "ID")]
    pub retry_request: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Doctor,
    Status,
    Skills(env_commands::SkillsArgs),
    Log(env_commands::LogArgs),
    Board(env_commands::BoardArgs),
    Usage(env_commands::UsageArgs),
    Hook(env_commands::HookArgs),
    RunCreate(runs::RunCreateArgs),
    RunUse(runs::RunUseArgs),
    RunCurrent,
    RunList,
    RunShow(runs::RunShowArgs),
    TaskCreate(tasks::TaskCreateArgs),
    TaskList(tasks::TaskListArgs),
    TaskUpdate(tasks::TaskUpdateArgs),
    WorkerStart(workers::WorkerStartArgs),
    WorkerShow(workers::DispatchIdArgs),
    WorkerRead(workers::WorkerReadArgs),
    WorkerList(workers::WorkerListArgs),
    WorkerStop(workers::DispatchIdArgs),
    WorkerAbandon(workers::DispatchIdArgs),
    WorkerRetain(workers::WorkerRetainArgs),
    WorkerRelease(workers::DispatchIdArgs),
    WorkerGc(workers::WorkerGcArgs),
    Dispatch(workers::DispatchArgs),
    DispatchShow(workers::DispatchShowArgs),
    Send(messages::SendArgs),
    Check(messages::CheckArgs),
    Ask(messages::AskArgs),
    Reply(messages::ReplyArgs),
    Inbox(messages::InboxArgs),
    GateCreate(gates::GateCreateArgs),
    GateResolve(gates::GateResolveArgs),
    GateList(gates::GateListArgs),
    RequestShow(recovery::RequestShowArgs),
    Reset(recovery::ResetArgs),
}

/// What every command handler receives: the global flags, plus constructors
/// for the runtime pieces. Handlers live in `commands/*.rs`, one function per
/// command, so each later task edits only the file it owns.
pub struct Ctx {
    pub json: bool,
    pub retry_request: Option<String>,
}

impl Ctx {
    /// Process environment lookup, with empty values treated as unset.
    pub fn env(name: &str) -> Option<String> {
        env::var(name).ok().filter(|value| !value.is_empty())
    }

    pub fn paths(&self) -> Result<Paths> {
        paths::resolve(&Self::env)
    }

    /// The herdr client: `herdr::socket::open_from_env`.
    pub fn herdr(&self) -> Result<Box<dyn Herdr>> {
        socket::open_from_env(&Self::env)
    }

    /// Loads `config.toml`. A missing default file means all defaults; a
    /// `POD_CONFIG` that names a missing file is a usage error (config.md).
    pub fn config(&self, paths: &Paths) -> Result<Config> {
        if Self::env("POD_CONFIG").is_some() && !paths.config_path.exists() {
            return Err(PodError::usage(format!(
                "POD_CONFIG names a missing file: {}",
                paths.config_path.display()
            )));
        }
        Config::load(&paths.config_path)
    }

    /// The full runtime for a command: paths, config, store, herdr, system
    /// clock, and the resolved caller (`None` outside herdr).
    pub fn engine(&self) -> Result<Engine> {
        self.engine_with(false)
    }

    /// The runtime for a read-only command: as `engine`, but a caller that
    /// cannot be resolved (herdr down) is `None` instead of an error, so
    /// `--run` reads still work (spec 3.4).
    pub fn read_engine(&self) -> Result<Engine> {
        self.engine_with(true)
    }

    fn engine_with(&self, best_effort_caller: bool) -> Result<Engine> {
        let paths = self.paths()?;
        let config = self.config(&paths)?;
        let store = Store::open(&paths)?;
        let herdr = self.herdr()?;
        let caller = match identity::resolve(&Self::env, herdr.as_ref()) {
            Err(_) if best_effort_caller => None,
            caller => caller?,
        };
        Ok(Engine::new(
            store,
            herdr,
            Box::new(SystemClock),
            config,
            paths,
            caller,
        ))
    }
}

pub fn run() -> Result<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error
                .print()
                .map_err(|io_error| PodError::usage(io_error.to_string()))?;
            return Ok(());
        }
        Err(error) => return Err(PodError::usage(error.to_string())),
    };
    let ctx = Ctx {
        json: cli.json,
        retry_request: cli.retry_request,
    };
    match cli.command {
        Command::Doctor => env_commands::doctor(&ctx),
        Command::Status => env_commands::status(&ctx),
        Command::Skills(args) => env_commands::skills(&ctx, args),
        Command::Log(args) => env_commands::log(&ctx, args),
        Command::Board(args) => env_commands::board(&ctx, args),
        Command::Usage(args) => env_commands::usage(&ctx, args),
        Command::Hook(args) => env_commands::hook(&ctx, args),
        Command::RunCreate(args) => runs::run_create(&ctx, args),
        Command::RunUse(args) => runs::run_use(&ctx, args),
        Command::RunCurrent => runs::run_current(&ctx),
        Command::RunList => runs::run_list(&ctx),
        Command::RunShow(args) => runs::run_show(&ctx, args),
        Command::TaskCreate(args) => tasks::task_create(&ctx, args),
        Command::TaskList(args) => tasks::task_list(&ctx, args),
        Command::TaskUpdate(args) => tasks::task_update(&ctx, args),
        Command::WorkerStart(args) => workers::worker_start(&ctx, args),
        Command::WorkerShow(args) => workers::worker_show(&ctx, args),
        Command::WorkerRead(args) => workers::worker_read(&ctx, args),
        Command::WorkerList(args) => workers::worker_list(&ctx, args),
        Command::WorkerStop(args) => workers::worker_stop(&ctx, args),
        Command::WorkerAbandon(args) => workers::worker_abandon(&ctx, args),
        Command::WorkerRetain(args) => workers::worker_retain(&ctx, args),
        Command::WorkerRelease(args) => workers::worker_release(&ctx, args),
        Command::WorkerGc(args) => workers::worker_gc(&ctx, args),
        Command::Dispatch(args) => workers::dispatch(&ctx, args),
        Command::DispatchShow(args) => workers::dispatch_show(&ctx, args),
        Command::Send(args) => messages::send(&ctx, args),
        Command::Check(args) => messages::check(&ctx, args),
        Command::Ask(args) => messages::ask(&ctx, args),
        Command::Reply(args) => messages::reply(&ctx, args),
        Command::Inbox(args) => messages::inbox(&ctx, args),
        Command::GateCreate(args) => gates::gate_create(&ctx, args),
        Command::GateResolve(args) => gates::gate_resolve(&ctx, args),
        Command::GateList(args) => gates::gate_list(&ctx, args),
        Command::RequestShow(args) => recovery::request_show(&ctx, args),
        Command::Reset(args) => recovery::reset(&ctx, args),
    }
}

pub fn json_requested() -> bool {
    env::args_os().any(|arg| arg == "--json")
}

pub fn clap_command() -> clap::Command {
    Cli::command()
}
