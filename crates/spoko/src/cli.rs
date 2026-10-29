use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::commands::{self, Ctx};

#[derive(Parser)]
#[command(name = "spoko", version, about = "Synchronizacja pracy w toku między Twoimi komputerami")]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Zaloguj ten komputer (z tokenem: pierwsze urządzenie / odzyskiwanie; bez: parowanie).
    Login {
        server: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        recovery_key: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Urządzenia konta.
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,
    },
    /// Nowy projekt z katalogu (domyślnie bieżącego).
    Init {
        path: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Istniejący projekt do katalogu (pusty: checkout; niepusty: dołączenie).
    Clone { project: String, path: Option<PathBuf> },
    /// Jednorazowa synchronizacja (bieżący projekt albo wszystkie).
    Sync,
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Lista konfliktów albo rozwiązanie jednego.
    Resolve {
        path: Option<String>,
        #[arg(long)]
        done: bool,
    },
    Log {
        path: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Przywróć plik: `--at <commit|unix-czas|10m|2h|3d>` albo `--from-join`.
    Restore {
        path: String,
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        from_join: bool,
    },
    Pause,
    Resume {
        #[arg(long)]
        confirm: bool,
    },
    /// Wolumeny Dockera.
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    Daemon {
        #[command(subcommand)]
        cmd: Option<DaemonCmd>,
    },
}

#[derive(Subcommand)]
pub enum DeviceCmd {
    /// Sparuj ten komputer z kontem (zatwierdzenie na innym urządzeniu).
    Add { server: String, #[arg(long)] name: Option<String> },
    /// Zatwierdź parowanie kodem pokazanym na nowym urządzeniu.
    Approve { code: String, #[arg(long)] yes: bool },
    List { #[arg(long)] json: bool },
    /// Odbierz dostęp (rotuje klucz, wypisuje nowy klucz odzyskiwania).
    Revoke { device: String },
}

#[derive(Subcommand)]
pub enum VolumeCmd {
    Push { name: Option<String> },
    Pull { name: Option<String>, #[arg(long)] fresh: bool, #[arg(long)] force: bool },
}

#[derive(Subcommand)]
pub enum HooksCmd {
    Run,
    Dismiss,
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Co jest synchronizowane, ignorowane, binarne, obserwowane.
    Check,
}

#[derive(Subcommand)]
pub enum DaemonCmd {
    Run,
    Start,
    Stop,
    Install,
    Uninstall,
}

/// Runs the CLI; returns the process exit code.
pub fn main_with(args: impl IntoIterator<Item = impl Into<OsString> + Clone>, out: &mut dyn Write) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    let mut ctx = Ctx::from_env();
    match commands::run(&mut ctx, cli.cmd, out) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("błąd: {e}");
            1
        }
    }
}
