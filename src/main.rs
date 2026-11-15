mod applier;
mod config;
mod http;
mod lock;
mod pool;
mod source;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rand::RngExt;

use crate::applier::Applier;
use crate::config::{Config, SelectMode};
use crate::http::{Http, Log};
use crate::lock::PoolLock;
use crate::pool::Pool;
use crate::source::Source;

#[derive(Parser)]
#[command(
    name = "wallpaper-guardian",
    version,
    about = "Guardian photos-of-the-day wallpaper pool and slideshow",
    long_about = "Fetch gallery photos from a Guardian series, keep a rolling pool, and \
                  apply one photo at a time through any wallpaper command. Designed to run \
                  from systemd timers, a keybind, or by hand."
)]
struct Cli {
    /// Configuration file. Default: $WALLPAPER_GUARDIAN_CONFIG, then
    /// ~/.config/wallpaper-guardian/config.toml, then built-in defaults.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Log per-photo downloads and deck decisions.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Refresh the pool from the source feeds and prune expired photos
    Fetch,
    /// Apply a photo that has not been shown this cycle
    Next,
    /// Apply a specific photo file
    Apply {
        /// Path to the image to apply
        file: PathBuf,
    },
    /// Print pool statistics
    Status,
    /// Print the effective configuration as TOML and exit
    Config,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wallpaper-guardian: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let config = Config::load(cli.config.as_deref())?;
    let log = Log::new(cli.verbose);

    match cli.command {
        Command::Config => {
            print!("{}", config.to_toml()?);
            return Ok(());
        }
        Command::Status => return status(&config),
        _ => {}
    }

    let http = Http::new(&config.source)?;
    let source = Source::new(&config.source, http.clone(), log)?;
    let pool = Pool::open(&config.pool, &config.caption)?;
    let applier = Applier::new(&config.apply, log);

    match cli.command {
        Command::Fetch => {
            let Some(_lock) = lock(&pool, log)? else {
                return Ok(());
            };
            fetch(&config, &source, &pool, &http, log)
        }
        Command::Next => {
            let Some(_lock) = lock(&pool, log)? else {
                return Ok(());
            };
            next(&config, &source, &pool, &http, &applier, log)
        }
        Command::Apply { file } => apply(&config, &pool, &applier, &file, log),
        Command::Status | Command::Config => unreachable!(),
    }
}

fn lock(pool: &Pool, log: Log) -> Result<Option<PoolLock>> {
    match PoolLock::acquire(pool.dir())? {
        Some(lock) => Ok(Some(lock)),
        None => {
            log.info("another run is in progress, skipping");
            Ok(None)
        }
    }
}

fn status(config: &Config) -> Result<()> {
    let pool = Pool::open(&config.pool, &config.caption)?;
    let files = pool.list()?;
    let used = pool.used();
    let shown = used.iter().filter(|used| files.contains(used)).count();
    let current = pool
        .current()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());

    println!("pool: {} photos in {}", files.len(), pool.dir().display());
    println!("size: {:.1}M", pool.size()? as f64 / 1024.0 / 1024.0);
    println!("current: {current}");
    println!("used: {shown}/{} this cycle", files.len());
    Ok(())
}

fn fetch(config: &Config, source: &Source, pool: &Pool, http: &Http, log: Log) -> Result<()> {
    let photos = source
        .fetch()
        .context("fetching galleries (pool left untouched)")?;
    pool.index_meta(&photos)?;
    let saved = pool.save(&photos, http, log)?;
    let pruned = pool.prune(config.pool.retention_days)?;
    log.info(format!(
        "{saved} new photos, {pruned} pruned, pool is now {} files",
        pool.count()?
    ));
    Ok(())
}

fn next(
    config: &Config,
    source: &Source,
    pool: &Pool,
    http: &Http,
    applier: &Applier,
    log: Log,
) -> Result<()> {
    let mut files = pool.list()?;
    if files.is_empty() {
        log.info("pool empty, fetching...");
        fetch(config, source, pool, http, log)?;
        files = pool.list()?;
    }
    if files.is_empty() {
        bail!("pool still empty after fetch");
    }

    let pick = match config.select.mode {
        SelectMode::Deck => pick_deck(pool, &files, log)?,
        SelectMode::Random => pick_random(&files, pool.current().as_ref()),
    };

    // Advance the deck before applying: a broken applier should not make the
    // same photo the only candidate forever.
    pool.mark_used(&pick)?;
    apply(config, pool, applier, &pick, log)
}

fn pick_deck(pool: &Pool, files: &[PathBuf], log: Log) -> Result<PathBuf> {
    let current = pool.current();
    let used = pool.used();
    let mut candidates: Vec<PathBuf> = files
        .iter()
        .filter(|file| !used.contains(*file))
        .cloned()
        .collect();

    if candidates.is_empty() {
        log.debug("deck exhausted, starting a new cycle");
        pool.reset_used()?;
        if let Some(current) = current.as_ref() {
            pool.mark_used(current)?;
        }
        candidates = files
            .iter()
            .filter(|file| Some(*file) != current.as_ref())
            .cloned()
            .collect();
    }
    if candidates.is_empty() {
        candidates = files.to_vec();
    }
    Ok(candidates[rand::rng().random_range(0..candidates.len())].clone())
}

fn pick_random(files: &[PathBuf], current: Option<&PathBuf>) -> PathBuf {
    let candidates: Vec<&PathBuf> = files.iter().filter(|file| Some(*file) != current).collect();
    let choices = if candidates.is_empty() {
        files.iter().collect()
    } else {
        candidates
    };
    choices[rand::rng().random_range(0..choices.len())].clone()
}

fn apply(config: &Config, pool: &Pool, applier: &Applier, file: &Path, log: Log) -> Result<()> {
    let file = fs::canonicalize(file).with_context(|| format!("cannot find {}", file.display()))?;
    if !applier.ensure()? {
        bail!(
            "no compositor session: `{}` probe failed",
            config.apply.command
        );
    }
    applier.apply(&file)?;
    pool.set_current(&file)?;
    pool.write_caption(&file)?;
    log.info(format!("applied {}", file.display()));
    Ok(())
}
