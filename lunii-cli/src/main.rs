//! `lunii-cli`: command line front-end of `lunii-core`.

use std::collections::BTreeSet;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Parser, Subcommand};
use lunii_core::constants::*;
use lunii_core::{Aborted, Context, Device, Level, Reporter, db, device, firmware, tools};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "lunii-cli",
    version,
    about = "Manage your Lunii / Flam storyteller"
)]
struct Cli {
    /// Mount point of the storyteller (default: first device found)
    #[arg(short, long, global = true)]
    device: Option<PathBuf>,

    /// Show debug messages
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Do not download the official story database
    #[arg(long, global = true)]
    offline: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the storytellers connected to this computer
    Devices,
    /// Show device details (firmware, SNU, keys)
    Info,
    /// List installed stories
    List {
        /// Compute the size of each story
        #[arg(long)]
        sizes: bool,
    },
    /// Import story archives (files or directories)
    Import {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Export stories to a directory
    Export {
        /// Stories (full or partial UUID)
        uuids: Vec<String>,
        /// Export all stories
        #[arg(long, conflicts_with = "uuids")]
        all: bool,
        /// Output directory
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Remove stories from the device
    Remove {
        #[arg(required = true)]
        uuids: Vec<String>,
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Toggle the hidden state of stories
    Hide {
        #[arg(required = true)]
        uuids: Vec<String>,
    },
    /// Toggle the night mode flag of stories (Lunii only)
    NightMode {
        #[arg(required = true)]
        uuids: Vec<String>,
    },
    /// Move stories in the list
    Move(MoveArgs),
    /// Find (or recover) stories present on the device but missing from the index
    Recover {
        /// Only list the lost stories
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete story directories missing from the index
    Cleanup {
        #[arg(short, long)]
        yes: bool,
    },
    /// Show or change night mode settings (Lunii only)
    NmConfig(NmConfigArgs),
    /// Story databases
    #[command(subcommand)]
    Db(DbCommand),
    /// Download the firmware of the device from the Luniistore
    Firmware {
        /// Luniistore login (the password is read from LUNII_PASSWORD or prompted)
        #[arg(long)]
        email: String,
        /// Output directory
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
    },
    /// Check if a new version of Lunii is available
    CheckUpdate,
    /// Download FFMPEG (Windows / macOS) into ~/.lunii
    InstallFfmpeg,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct MoveDirection {
    /// One position up
    #[arg(long)]
    up: bool,
    /// One position down
    #[arg(long)]
    down: bool,
    /// To the top of the list
    #[arg(long)]
    top: bool,
    /// To the bottom of the list
    #[arg(long)]
    bottom: bool,
}

#[derive(Args)]
struct MoveArgs {
    #[arg(required = true)]
    uuids: Vec<String>,
    #[command(flatten)]
    direction: MoveDirection,
}

#[derive(Args)]
struct NmConfigArgs {
    /// Enable night mode
    #[arg(long)]
    enable: Option<bool>,
    /// Limit volume
    #[arg(long)]
    volume_limit: Option<bool>,
    /// Number of stories to play
    #[arg(long)]
    stories: Option<u16>,
    /// Autoplay
    #[arg(long)]
    autoplay: Option<bool>,
    /// Turn off night mode at the end
    #[arg(long)]
    turnoff: Option<bool>,
}

#[derive(Subcommand)]
enum DbCommand {
    /// Download again the official database
    Refresh,
    /// Import a STUdio database (unofficial.json)
    ImportStudio { file: PathBuf },
    /// Database statistics
    Info,
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

struct CliReporter {
    verbose: bool,
    tty: bool,
    progress_shown: AtomicBool,
}

impl CliReporter {
    fn clear_progress(&self) {
        if self.progress_shown.swap(false, Ordering::SeqCst) {
            eprint!("\r\x1b[2K");
        }
    }
}

impl Reporter for CliReporter {
    fn log(&self, level: Level, message: &str) {
        if level == Level::Debug && !self.verbose || level == Level::Trace {
            return;
        }
        self.clear_progress();
        let prefix = match level {
            Level::Error => "ERROR ",
            Level::Warn => "WARN  ",
            Level::Debug => "DEBUG ",
            _ => "",
        };
        eprintln!("{prefix}{message}");
    }

    fn story_progress(&self, short_uuid: &str, current: usize, total: usize) {
        if self.tty && total > 0 {
            eprint!("\r\x1b[2K{short_uuid} [{}/{total}]", current + 1);
            let _ = std::io::stderr().flush();
            self.progress_shown.store(true, Ordering::SeqCst);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open(ctx: &Context, path: Option<&Path>) -> Result<Box<dyn Device>> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => device::find_devices(None)
            .into_iter()
            .next()
            .context("No storyteller found. Plug it in or use --device <PATH>.")?,
    };
    device::open_device(ctx, &path)
}

fn confirm(question: &str) -> Result<bool> {
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_lowercase().as_str(),
        "y" | "yes" | "o" | "oui"
    ))
}

fn resolve(dev: &dyn Device, uuids: &[String]) -> Result<Vec<Uuid>> {
    uuids
        .iter()
        .map(|u| dev.find_story(u).map(|s| s.uuid))
        .collect()
}

fn human_size(size: u64) -> String {
    format!("{:.1} MB", size as f64 / 1024.0 / 1024.0)
}

/// Archives to import, in the given order: files as given, directories scanned
/// recursively (sorted by name). Duplicates are dropped.
fn collect_archives(paths: &[PathBuf], extensions: &[&str]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for p in paths {
        let found: Vec<PathBuf> = if p.is_dir() {
            walkdir::WalkDir::new(p)
                .sort_by_file_name()
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name().to_string_lossy().to_lowercase();
                    e.file_type().is_file() && extensions.iter().any(|ext| name.ends_with(ext))
                })
                .map(|e| e.into_path())
                .collect()
        } else {
            vec![p.clone()]
        };
        out.extend(found.into_iter().filter(|f| seen.insert(f.clone())));
    }
    out
}

fn duration(start: Instant) -> String {
    let d = start.elapsed().as_secs();
    if d > 120 {
        format!("{} min {} s", d / 60, d % 60)
    } else {
        format!("{d} s")
    }
}

/// Runs a batch, stopping on abort, and reports the summary.
fn batch<T>(ctx: &Context, items: &[T], mut f: impl FnMut(&T) -> Result<()>) -> Result<usize> {
    let mut success = 0;
    for item in items {
        if ctx.is_aborted() {
            return Err(Aborted.into());
        }
        match f(item) {
            Ok(()) => success += 1,
            Err(e) if e.is::<Aborted>() => return Err(e),
            Err(e) => ctx.error(format!("{e:#}")),
        }
    }
    Ok(success)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn run(cli: Cli, ctx: &Context) -> Result<()> {
    let needs_db = !matches!(
        cli.command,
        Command::Devices | Command::CheckUpdate | Command::InstallFfmpeg | Command::Db(_)
    );
    if needs_db {
        if cli.offline {
            let _ = ensure_cfg_dir();
            if !file_official_db().is_file() {
                std::fs::write(file_official_db(), "{}")?;
            }
        }
        if !db::load_db(false) {
            ctx.warn("⚠️ Unable to fetch the official story database (network error ?)");
        }
    }

    match cli.command {
        Command::Devices => {
            let devices = device::find_devices(cli.device.as_deref());
            if devices.is_empty() {
                println!("No storyteller found.");
            }
            for d in devices {
                let kind = if device::is_lunii(&d) {
                    "Lunii"
                } else {
                    "Flam"
                };
                println!("{kind}\t{}", d.display());
            }
        }

        Command::Info => {
            let dev = open(ctx, cli.device.as_deref())?;
            println!("{}", dev.info());
            println!("- type     : {}", dev.version());
            if !dev.can_import() {
                println!(
                    "⚠️ No story keys for this Lunii v3, import is not possible (see README)."
                );
            }
            if let Some(lunii) = dev.as_lunii() {
                println!(
                    "- night mode : {}",
                    if lunii.night_mode_enabled() {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
            }
        }

        Command::List { sizes } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            if sizes {
                dev.compute_sizes(ctx)?;
            }
            for (i, s) in dev.stories().iter().enumerate() {
                let flags = format!(
                    "{}{}{}",
                    if s.is_official() { "O" } else { "-" },
                    if s.hidden { "H" } else { "-" },
                    if s.nm { "N" } else { "-" }
                );
                let size = s.size.map(human_size).unwrap_or_default();
                println!(
                    "{:>3}  {}  {flags}  {:<50} {size}",
                    i + 1,
                    s.str_uuid(),
                    s.name()
                );
            }
            eprintln!(
                "{} stories (O: official, H: hidden, N: night mode)",
                dev.stories().len()
            );
        }

        Command::Import { paths } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            if !dev.can_import() {
                bail!("No story keys for this Lunii v3, import is not possible (see README).");
            }
            let archives = collect_archives(&paths, dev.supported_extensions());
            if dev.version() == DeviceVersion::FlamV1 && !archives.is_empty() {
                ctx.info(
                    "😮‍💨 This process is veeeeeeeeery long due to Flam firmware. 😴 Be patient ...",
                );
            }
            ctx.info(format!("Importing {} archives...", archives.len()));
            let total = archives.len();
            let success = batch(ctx, &archives, |file| {
                let start = Instant::now();
                match dev.import_story(ctx, file) {
                    Ok(()) => {
                        ctx.info(format!("Time to import : {}", duration(start)));
                        ctx.info(format!("👍 New story imported : '{}'", file.display()));
                        Ok(())
                    }
                    Err(e) => Err(e.context(format!("🛑 Failed to import : '{}'", file.display()))),
                }
            })?;
            ctx.info(format!("✅ Import done : {success}/{total}"));
        }

        Command::Export { uuids, all, out } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            std::fs::create_dir_all(&out)?;
            let targets: Vec<String> = if all {
                dev.stories().iter().map(|s| s.str_uuid()).collect()
            } else if uuids.is_empty() {
                bail!("Give story UUIDs or --all");
            } else {
                uuids
            };
            let total = targets.len();
            let success = batch(ctx, &targets, |uuid| {
                let story = dev.find_story(uuid)?;
                match dev.export_story(ctx, uuid, &out) {
                    Ok(path) => {
                        ctx.info(format!("👍 Story exported to '{}'", path.display()));
                        Ok(())
                    }
                    Err(e) => Err(e.context(format!("🛑 Failed to export : '{}'", story.name()))),
                }
            })?;
            ctx.info(format!("✅ Export done : {success}/{total}"));
        }

        Command::Remove { uuids, yes } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            let stories: Vec<_> = uuids
                .iter()
                .map(|u| dev.find_story(u))
                .collect::<Result<_>>()?;
            eprintln!("You are requesting to delete :");
            for s in &stories {
                eprintln!("- {}", s.name());
            }
            if !yes && !confirm("Do you want to proceed ?")? {
                return Ok(());
            }
            let success = batch(ctx, &stories, |s| {
                dev.remove_story(ctx, &s.str_uuid())?;
                ctx.info(format!("👍 Story removed: '{}'", s.name()));
                Ok(())
            })?;
            ctx.info(format!("✅ {success} stories removed"));
        }

        Command::Hide { uuids } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            let ids = resolve(dev.as_ref(), &uuids)?;
            dev.toggle_hidden(ctx, &ids)?;
            ctx.info("✅ Stories updated...");
        }

        Command::NightMode { uuids } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            let ids = resolve(dev.as_ref(), &uuids)?;
            dev.as_lunii_mut()
                .context("Night mode is only available on Lunii")?
                .toggle_night_mode(&ids)?;
            ctx.info("✅ Stories updated...");
        }

        Command::Move(args) => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            let ids = resolve(dev.as_ref(), &args.uuids)?;
            let d = args.direction;
            let offset = if d.up {
                -1
            } else if d.down {
                1
            } else if d.top {
                -2
            } else {
                2
            };
            dev.move_stories(&ids, offset)?;
            ctx.info("✅ Stories updated...");
        }

        Command::Recover { dry_run } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            ctx.info("🚧 Analyzing storage ...");
            let count = dev.recover_stories(ctx, dry_run)?;
            let what = if dry_run {
                "found (try to recover them)"
            } else {
                "recovered"
            };
            ctx.info(format!("✅ Storage parsed, {count} lost stories {what}"));
        }

        Command::Cleanup { yes } => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            if !yes && !confirm("Delete every story directory missing from the index ?")? {
                return Ok(());
            }
            ctx.info("🚧 Cleaning storage ...");
            let (count, size) = dev.cleanup_stories(ctx)?;
            ctx.info(format!(
                "✅ Storage parsed, {count} lost stories removed, {size} MB recovered"
            ));
        }

        Command::NmConfig(args) => {
            let mut dev = open(ctx, cli.device.as_deref())?;
            let lunii = dev
                .as_lunii_mut()
                .context("Night mode settings are only available on Lunii")?;
            let mut changed = false;
            let mut set = |pos: usize, v: Option<u16>| {
                if let Some(v) = v {
                    lunii.config[pos] = v;
                    changed = true;
                }
            };
            set(LUNII_CFGPOS_NM_ENABLED, args.enable.map(u16::from));
            set(
                LUNII_CFGPOS_NM_VOL_LIMIT,
                args.volume_limit.map(|b| if b { 30 } else { 0 }),
            );
            set(LUNII_CFGPOS_NM_STORYCOUNT, args.stories);
            set(LUNII_CFGPOS_NM_AUTOPLAY, args.autoplay.map(u16::from));
            set(LUNII_CFGPOS_NM_TURNOFF_NM, args.turnoff.map(u16::from));
            if changed {
                lunii.update_config()?;
                ctx.info("✅ Night mode settings saved");
            }
            let c = &lunii.config;
            println!("enabled      : {}", c[LUNII_CFGPOS_NM_ENABLED] != 0);
            println!("volume limit : {}", c[LUNII_CFGPOS_NM_VOL_LIMIT] != 0);
            println!("stories      : {}", c[LUNII_CFGPOS_NM_STORYCOUNT]);
            println!("autoplay     : {}", c[LUNII_CFGPOS_NM_AUTOPLAY] != 0);
            println!("turn off     : {}", c[LUNII_CFGPOS_NM_TURNOFF_NM] != 0);
        }

        Command::Db(cmd) => match cmd {
            DbCommand::Refresh => {
                if db::load_db(true) {
                    ctx.info("✅ Official database updated");
                } else {
                    bail!("🛑 Failed to update the official database");
                }
            }
            DbCommand::ImportStudio { file } => {
                db::load_db(false);
                let (count, total) = tools::import_studio_db(ctx, &file)?;
                ctx.info(format!("✅ STUdio DB imported ({count}/{total})."));
            }
            DbCommand::Info => {
                db::load_db(false);
                let (official, third) = db::db_sizes();
                println!(
                    "official    : {official} stories ({})",
                    file_official_db().display()
                );
                println!(
                    "third-party : {third} stories ({})",
                    file_third_party_db().display()
                );
                println!(
                    "ffmpeg      : {}",
                    which_ffmpeg()
                        .map(|p| p.display().to_string())
                        .unwrap_or("not found".into())
                );
            }
        },

        Command::Firmware { email, out } => {
            let dev = open(ctx, cli.device.as_deref())?;
            let password = match std::env::var("LUNII_PASSWORD") {
                Ok(p) => p,
                Err(_) => rpassword::prompt_password("Luniistore password: ")?,
            };
            let auth = firmware::luniistore_get_authtoken(&email, &password)?
                .context("⚠️ Login failed, please check your credentials")?;
            ctx.info("Login success...");
            std::fs::create_dir_all(&out)?;
            let list = firmware::device_fw_getlist(dev.version(), &dev.snu_str(), &auth)?;
            for (index, name) in list.iter().enumerate() {
                let target = out.join(name);
                let size = firmware::device_fw_download(
                    dev.version(),
                    &dev.snu_str(),
                    &auth,
                    &target,
                    index != 0,
                )?;
                ctx.info(format!(
                    "✅ Firmware downloaded to {} ({size} bytes)",
                    target.display()
                ));
            }
        }

        Command::CheckUpdate => match tools::latest_version() {
            Some(v) => println!("Latest version: {v} (this one: v{})", lunii_core::VERSION),
            None => bail!("Unable to check for updates"),
        },

        Command::InstallFfmpeg => {
            let path = tools::install_ffmpeg(ctx)?;
            ctx.info(format!("✅ FFMPEG installed ({}).", path.display()));
        }
    }
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let reporter = Arc::new(CliReporter {
        verbose: cli.verbose,
        tty: std::io::stderr().is_terminal(),
        progress_shown: AtomicBool::new(false),
    });
    let ctx = Context::new(reporter.clone());

    let abort = ctx.abort_handle();
    let _ = ctrlc::set_handler(move || {
        if abort.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
        eprintln!("\nAbort requested, finishing current step... (Ctrl-C again to force)");
    });

    if let Err(e) = run(cli, &ctx) {
        reporter.clear_progress();
        if e.is::<Aborted>() {
            eprintln!("🛑 Aborted");
            std::process::exit(130);
        }
        eprintln!("ERROR {e:#}");
        std::process::exit(1);
    }
    reporter.clear_progress();
}
