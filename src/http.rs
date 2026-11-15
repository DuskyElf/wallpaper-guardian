use std::fmt::Display;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::Client;

use crate::config::SourceConfig;

#[derive(Clone, Copy, Debug)]
pub struct Log {
    verbose: bool,
}

impl Log {
    pub fn new(verbose: bool) -> Self {
        Self { verbose }
    }

    pub fn info(&self, message: impl Display) {
        eprintln!("wallpaper-guardian: {message}");
    }

    pub fn debug(&self, message: impl Display) {
        if self.verbose {
            eprintln!("wallpaper-guardian: {message}");
        }
    }
}

/// Small blocking HTTP client: browser user agent, one timeout per request
/// kind, retries with linear backoff. Returns the body of a 2xx response.
#[derive(Clone)]
pub struct Http {
    client: Client,
    attempts: u32,
    timeout: Duration,
    download_timeout: Duration,
}

impl Http {
    pub fn new(config: &SourceConfig) -> Result<Self> {
        // rustls needs a process-wide crypto provider; installation fails
        // harmlessly when another caller already did it.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .user_agent(&config.user_agent)
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            client,
            attempts: config.attempts.max(1),
            timeout: Duration::from_secs(config.timeout_secs),
            download_timeout: Duration::from_secs(config.download_timeout_secs),
        })
    }

    pub fn text(&self, url: &str) -> Result<String> {
        let body = self.get(url, self.timeout)?;
        String::from_utf8(body).with_context(|| format!("{url} returned non-UTF-8 text"))
    }

    pub fn bytes(&self, url: &str) -> Result<Vec<u8>> {
        self.get(url, self.download_timeout)
    }

    fn get(&self, url: &str, timeout: Duration) -> Result<Vec<u8>> {
        let mut last_error = None;
        for attempt in 0..self.attempts {
            let request = self.client.get(url).timeout(timeout);
            match request
                .send()
                .and_then(|response| response.error_for_status())
            {
                Ok(response) => match response.bytes() {
                    Ok(body) => return Ok(body.to_vec()),
                    Err(error) => last_error = Some(error.to_string()),
                },
                Err(error) => last_error = Some(error.to_string()),
            }
            if attempt + 1 < self.attempts {
                std::thread::sleep(Duration::from_millis(1000 * u64::from(attempt + 1)));
            }
        }
        Err(anyhow!(
            "{url}: {}",
            last_error.unwrap_or_else(|| "no response".to_string())
        ))
    }
}
