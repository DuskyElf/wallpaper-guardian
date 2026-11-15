use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::ApplyConfig;
use crate::http::Log;

/// The wallpaper-applier seam. The command, its arguments and the daemon
/// probe all come from configuration, so awww, swww, feh or a script are the
/// same code path.
pub struct Applier {
    config: ApplyConfig,
    log: Log,
}

impl Applier {
    pub fn new(config: &ApplyConfig, log: Log) -> Self {
        Self {
            config: config.clone(),
            log,
        }
    }

    /// Make sure the compositor is reachable. When `ensure_daemon` is on,
    /// probe it, spawn the daemon if missing, then poll until it answers.
    pub fn ensure(&self) -> Result<bool> {
        if !self.config.ensure_daemon {
            return Ok(true);
        }
        if self.query_ok() {
            return Ok(true);
        }

        self.log
            .info(format!("starting {}", self.config.daemon_command));
        let mut command = Command::new(&self.config.daemon_command);
        command
            .args(&self.config.daemon_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Detach from the unit's process group so the daemon outlives a
        // oneshot service and does not receive its signals.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        if let Err(error) = command.spawn() {
            self.log.debug(format!(
                "cannot start {}: {error}",
                self.config.daemon_command
            ));
            return Ok(false);
        }

        for _ in 0..self.config.daemon_tries {
            sleep(Duration::from_millis(self.config.daemon_wait_ms));
            if self.query_ok() {
                return Ok(true);
            }
        }
        Ok(self.query_ok())
    }

    pub fn apply(&self, file: &Path) -> Result<()> {
        let file = file.to_string_lossy();
        let args: Vec<String> = self
            .config
            .args
            .iter()
            .map(|argument| argument.replace("{file}", &file))
            .collect();
        let status = Command::new(&self.config.command)
            .args(&args)
            .status()
            .with_context(|| format!("running `{}`", self.config.command))?;
        if !status.success() {
            bail!("`{}` exited with {status}", self.config.command);
        }
        Ok(())
    }

    fn query_ok(&self) -> bool {
        Command::new(&self.config.command)
            .args(&self.config.query_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}
