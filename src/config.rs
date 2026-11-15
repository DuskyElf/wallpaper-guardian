use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const ENV_CONFIG: &str = "WALLPAPER_GUARDIAN_CONFIG";
pub const DEFAULT_SERIES: &str =
    "https://www.theguardian.com/news/series/ten-best-photographs-of-the-day";
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

/// The whole on-disk configuration. Every field has a default, so an empty
/// file is valid and `wallpaper-guardian config` prints the effective values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub pool: PoolConfig,
    pub source: SourceConfig,
    pub apply: ApplyConfig,
    pub select: SelectConfig,
    pub caption: CaptionConfig,
}

impl Config {
    /// Load the explicit path, then `$WALLPAPER_GUARDIAN_CONFIG`, then
    /// `$XDG_CONFIG_HOME/wallpaper-guardian/config.toml`. A missing default
    /// file means "all defaults"; a missing explicit file is an error.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(path) => Some(expand_tilde(path)),
            None => match env::var_os(ENV_CONFIG) {
                Some(path) => Some(expand_tilde(Path::new(&path))),
                None => {
                    let path = default_config_path();
                    path.exists().then_some(path)
                }
            },
        };

        let mut config = match path {
            Some(path) => {
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("reading config {}", path.display()))?;
                toml::from_str(&text)
                    .with_context(|| format!("parsing config {}", path.display()))?
            }
            None => Self::default(),
        };
        config.resolve()?;
        config.validate()?;
        Ok(config)
    }

    fn resolve(&mut self) -> Result<()> {
        let dir = expand_tilde(&self.pool.dir);
        self.pool.dir = std::path::absolute(&dir)
            .with_context(|| format!("resolving pool.dir {}", dir.display()))?;
        self.caption.file = expand_tilde(&self.caption.file);
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.pool.dir.as_os_str().is_empty() {
            bail!("pool.dir must not be empty");
        }
        if self.pool.retention_days == 0 {
            bail!("pool.retention_days must be at least 1");
        }
        if self.source.series.is_empty() {
            bail!("source.series must list at least one gallery series URL");
        }
        if self
            .source
            .series
            .iter()
            .any(|series| series.trim().is_empty())
        {
            bail!("source.series contains an empty URL");
        }
        if self.source.days == 0 {
            bail!("source.days must be at least 1");
        }
        if self.source.timeout_secs == 0 || self.source.download_timeout_secs == 0 {
            bail!("source timeouts must be at least 1 second");
        }
        if self.source.attempts == 0 || self.source.download_attempts == 0 {
            bail!("source attempts must be at least 1");
        }
        if self
            .source
            .media_hosts
            .iter()
            .any(|host| host.trim().is_empty())
        {
            bail!("source.media_hosts contains an empty host");
        }
        if self.apply.command.trim().is_empty() {
            bail!("apply.command must not be empty");
        }
        if !self
            .apply
            .args
            .iter()
            .any(|argument| argument.contains("{file}"))
        {
            bail!("apply.args must contain a {{file}} placeholder");
        }
        regex::Regex::new(&self.source.title_pattern)
            .context("source.title_pattern is not a valid regex")?;
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serializing the effective configuration")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoolConfig {
    /// Photo pool and state directory. `~` is expanded.
    pub dir: PathBuf,
    /// Delete photos whose mtime is older than this many days.
    pub retention_days: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            dir: default_pool_dir(),
            retention_days: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourceConfig {
    /// Gallery series pages. Each one is read at `<series>/rss`.
    pub series: Vec<String>,
    /// Only take gallery items published within this many days.
    pub days: u64,
    /// Skip media variants narrower than this many pixels.
    pub min_width: u32,
    pub user_agent: String,
    /// Timeout for feeds and gallery pages, in seconds.
    pub timeout_secs: u64,
    /// Timeout for photo downloads, in seconds.
    pub download_timeout_secs: u64,
    /// Attempts per feed or gallery request, including the first one.
    pub attempts: u32,
    /// Attempts per photo download, including the first one.
    pub download_attempts: u32,
    /// RSS links must contain this substring. Empty disables the filter.
    pub gallery_path: String,
    /// Only URLs containing one of these hosts are treated as photos.
    pub media_hosts: Vec<String>,
    /// RSS item titles that are galleries worth reading.
    pub title_pattern: String,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            series: vec![DEFAULT_SERIES.to_string()],
            days: 5,
            min_width: 600,
            user_agent: DEFAULT_USER_AGENT.to_string(),
            timeout_secs: 45,
            download_timeout_secs: 120,
            attempts: 2,
            download_attempts: 3,
            gallery_path: "/gallery/".to_string(),
            media_hosts: vec!["i.guim.co.uk".to_string()],
            title_pattern: r"(?i)photos? of the (day|weekend)".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApplyConfig {
    /// Wallpaper command. Defaults to awww.
    pub command: String,
    /// Arguments passed to `command`. `{file}` expands to the photo path.
    pub args: Vec<String>,
    /// Check for a live compositor and start the daemon when it is missing.
    pub ensure_daemon: bool,
    pub daemon_command: String,
    pub daemon_args: Vec<String>,
    /// Arguments that probe the compositor, usually `["query"]`.
    pub query_args: Vec<String>,
    /// How long to wait between daemon readiness probes.
    pub daemon_wait_ms: u64,
    /// How many readiness probes to make before giving up.
    pub daemon_tries: u32,
}

impl Default for ApplyConfig {
    fn default() -> Self {
        Self {
            command: "awww".to_string(),
            args: vec![
                "img".to_string(),
                "--transition-type".to_string(),
                "fade".to_string(),
                "--transition-duration".to_string(),
                "2".to_string(),
                "{file}".to_string(),
            ],
            ensure_daemon: true,
            daemon_command: "awww-daemon".to_string(),
            daemon_args: Vec::new(),
            query_args: vec!["query".to_string()],
            daemon_wait_ms: 300,
            daemon_tries: 10,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectMode {
    /// Show every photo once per cycle, then start a new cycle.
    #[default]
    Deck,
    /// Uniform random choice, avoiding an immediate repeat.
    Random,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectConfig {
    pub mode: SelectMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptionConfig {
    /// Overlay caption file, relative to pool.dir unless absolute.
    pub file: PathBuf,
    /// Template when the photo has a location.
    pub with_location: String,
    /// Template when it does not.
    pub without_location: String,
}

impl Default for CaptionConfig {
    fn default() -> Self {
        Self {
            file: PathBuf::from("caption.txt"),
            with_location: "{location}: {description}".to_string(),
            without_location: "{description}".to_string(),
        }
    }
}

fn home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn default_pool_dir() -> PathBuf {
    match env::var_os("XDG_CACHE_HOME") {
        Some(dir) => PathBuf::from(dir).join("wallpaper-guardian"),
        None => home_dir().join(".cache/wallpaper-guardian"),
    }
}

fn default_config_path() -> PathBuf {
    let base = match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => home_dir().join(".config"),
    };
    base.join("wallpaper-guardian/config.toml")
}

fn expand_tilde(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home_dir();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_is_all_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.pool.retention_days, 5);
        assert_eq!(config.source.days, 5);
        assert_eq!(config.source.min_width, 600);
        assert_eq!(config.select.mode, SelectMode::Deck);
        assert_eq!(config.caption.with_location, "{location}: {description}");
        assert_eq!(config.apply.command, "awww");
    }

    #[test]
    fn partial_sections_keep_other_defaults() {
        let config: Config = toml::from_str("[source]\ndays = 2\n").unwrap();
        assert_eq!(config.source.days, 2);
        assert_eq!(config.source.min_width, 600);
        assert_eq!(config.pool.retention_days, 5);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let error = toml::from_str::<Config>("[source]\ndya = 2\n").unwrap_err();
        assert!(error.to_string().contains("dya"), "{error}");
    }

    #[test]
    fn bad_regex_is_rejected() {
        let mut config = Config::default();
        config.source.title_pattern = "(".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = home_dir();
        assert_eq!(expand_tilde(Path::new("~/pictures")), home.join("pictures"));
        assert_eq!(
            expand_tilde(Path::new("/tmp/pool")),
            PathBuf::from("/tmp/pool")
        );
    }
}
