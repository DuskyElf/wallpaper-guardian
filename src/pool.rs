use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use regex::Regex;

use crate::config::{CaptionConfig, PoolConfig};
use crate::http::{Http, Log};
use crate::source::Photo;

static EXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/master/\d+\.(jpg|png)").unwrap());

/// The photo pool. Deep module over a directory: photo files, pruning,
/// the `current` marker, the deck's `used` log and the caption index.
pub struct Pool {
    dir: PathBuf,
    caption_file: PathBuf,
    caption: CaptionConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caption {
    pub location: String,
    pub description: String,
}

impl Pool {
    pub fn open(config: &PoolConfig, caption: &CaptionConfig) -> Result<Self> {
        fs::create_dir_all(&config.dir)
            .with_context(|| format!("creating pool directory {}", config.dir.display()))?;
        let caption_file = if caption.file.is_absolute() {
            caption.file.clone()
        } else {
            config.dir.join(&caption.file)
        };
        Ok(Self {
            dir: config.dir.clone(),
            caption_file,
            caption: caption.clone(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn list(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.dir)
            .with_context(|| format!("reading pool directory {}", self.dir.display()))?
        {
            let path = entry?.path();
            if is_photo(&path) {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self.list()?.len())
    }

    /// Download photos that are not already on disk at a non-zero size.
    /// A failed download is logged and skipped, never fatal.
    pub fn save(&self, photos: &[Photo], http: &Http, log: Log) -> Result<usize> {
        let mut saved = 0;
        for photo in photos {
            let path = self.dir.join(photo_filename(photo));
            if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 0) {
                continue;
            }
            match http.bytes(&photo.url) {
                Ok(bytes) if !bytes.is_empty() => {
                    fs::write(&path, bytes)
                        .with_context(|| format!("writing {}", path.display()))?;
                    saved += 1;
                    log.debug(format!("downloaded {} ({}px)", path.display(), photo.width));
                }
                Ok(_) => log.info(format!("empty download for {}", photo.url)),
                Err(error) => log.info(format!("download failed for {}: {error:#}", photo.url)),
            }
        }
        Ok(saved)
    }

    /// Remove photo files older than `retention_days` by mtime.
    pub fn prune(&self, retention_days: u64) -> Result<usize> {
        let max_age = Duration::from_secs(retention_days.saturating_mul(86_400));
        let cutoff = SystemTime::now()
            .checked_sub(max_age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut removed = 0;
        for path in self.list()? {
            let old = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .map(|modified| modified < cutoff)
                .unwrap_or(false);
            if old {
                match fs::remove_file(&path) {
                    Ok(()) => removed += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| format!("removing {}", path.display()));
                    }
                }
            }
        }
        Ok(removed)
    }

    pub fn current(&self) -> Option<PathBuf> {
        let text = fs::read_to_string(self.dir.join("current")).ok()?;
        let text = text.trim();
        (!text.is_empty()).then(|| PathBuf::from(text))
    }

    pub fn set_current(&self, file: &Path) -> Result<()> {
        write_atomic(
            &self.dir.join("current"),
            format!("{}\n", file.display()).as_bytes(),
        )
    }

    /// Photos already shown this deck cycle, in display order.
    pub fn used(&self) -> Vec<PathBuf> {
        fs::read_to_string(self.dir.join("used"))
            .map(|text| {
                text.lines()
                    .filter(|line| !line.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn mark_used(&self, file: &Path) -> Result<()> {
        let path = self.dir.join("used");
        let mut handle = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        writeln!(handle, "{}", file.display())
            .with_context(|| format!("appending {}", path.display()))
    }

    pub fn reset_used(&self) -> Result<()> {
        write_atomic(&self.dir.join("used"), b"")
    }

    /// Replace the caption index. Rows are `hash<TAB>location<TAB>description`
    /// and only photos with a description are kept, first non-empty wins.
    pub fn index_meta(&self, photos: &[Photo]) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        let mut rows = String::new();
        for photo in photos {
            if photo.description.is_empty() || !seen.insert(&photo.hash) {
                continue;
            }
            rows.push_str(&format!(
                "{}\t{}\t{}\n",
                photo.hash, photo.location, photo.description
            ));
        }
        write_atomic(&self.dir.join("captions.tsv"), rows.as_bytes())
    }

    pub fn meta(&self, hash: &str) -> Option<Caption> {
        let text = fs::read_to_string(self.dir.join("captions.tsv")).ok()?;
        for line in text.lines() {
            let mut parts = line.splitn(3, '\t');
            let candidate = parts.next()?;
            if candidate != hash {
                continue;
            }
            return Some(Caption {
                location: parts.next().unwrap_or_default().to_string(),
                description: parts.next().unwrap_or_default().to_string(),
            });
        }
        None
    }

    /// Refresh the overlay caption file for a photo. Removes it when the
    /// photo has no description.
    pub fn write_caption(&self, file: &Path) -> Result<()> {
        let hash = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(caption) = self.meta(&hash) else {
            return self.remove_caption();
        };
        if caption.description.is_empty() {
            return self.remove_caption();
        }
        let template = if caption.location.is_empty() {
            &self.caption.without_location
        } else {
            &self.caption.with_location
        };
        let text = template
            .replace("{location}", &caption.location)
            .replace("{description}", &caption.description);
        write_atomic(&self.caption_file, format!("{text}\n").as_bytes())
    }

    fn remove_caption(&self) -> Result<()> {
        match fs::remove_file(&self.caption_file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("removing {}", self.caption_file.display()))
            }
        }
    }

    pub fn size(&self) -> Result<u64> {
        fn walk(path: &Path) -> Result<u64> {
            let mut total = 0;
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let metadata = entry.metadata()?;
                if metadata.is_dir() {
                    total += walk(&entry.path())?;
                } else {
                    total += metadata.len();
                }
            }
            Ok(total)
        }
        walk(&self.dir)
    }
}

fn is_photo(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("png")
        })
}

fn photo_filename(photo: &Photo) -> String {
    let path = photo.url.split('?').next().unwrap_or(&photo.url);
    let extension = EXT_RE
        .captures(path)
        .map(|captures| captures[1].to_string())
        .unwrap_or_else(|| "jpg".to_string());
    format!("{}.{extension}", photo.hash)
}

/// Replace a small state file atomically so a reader never sees half a line.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
    ));
    {
        let mut handle = File::create(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        handle
            .write_all(contents)
            .with_context(|| format!("writing {}", temporary.display()))?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("renaming {} to {}", temporary.display(), path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caption_config() -> CaptionConfig {
        CaptionConfig {
            file: PathBuf::from("caption.txt"),
            with_location: "{location}: {description}".to_string(),
            without_location: "{description}".to_string(),
        }
    }

    fn pool(dir: &Path) -> Pool {
        Pool::open(
            &PoolConfig {
                dir: dir.to_path_buf(),
                retention_days: 5,
            },
            &caption_config(),
        )
        .unwrap()
    }

    #[test]
    fn caption_uses_location_only_when_present() {
        let temp = tempfile::tempdir().unwrap();
        let pool = pool(temp.path());
        let photo = Photo {
            hash: "a".repeat(40),
            url: "https://i.guim.co.uk/img/media/x/master/1.jpg?width=1020".to_string(),
            width: 1020,
            location: "Tokyo, Japan".to_string(),
            description: "A caption".to_string(),
        };
        pool.index_meta(std::slice::from_ref(&photo)).unwrap();
        let file = temp.path().join(format!("{}.jpg", photo.hash));
        fs::write(&file, b"x").unwrap();
        pool.write_caption(&file).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join("caption.txt")).unwrap(),
            "Tokyo, Japan: A caption\n"
        );

        let bare = Photo {
            location: String::new(),
            ..photo
        };
        pool.index_meta(std::slice::from_ref(&bare)).unwrap();
        pool.write_caption(&file).unwrap();
        assert_eq!(
            fs::read_to_string(temp.path().join("caption.txt")).unwrap(),
            "A caption\n"
        );
    }

    #[test]
    fn caption_file_is_removed_without_a_description() {
        let temp = tempfile::tempdir().unwrap();
        let pool = pool(temp.path());
        fs::write(temp.path().join("caption.txt"), "stale\n").unwrap();
        pool.write_caption(&temp.path().join(format!("{}.jpg", "b".repeat(40))))
            .unwrap();
        assert!(!temp.path().join("caption.txt").exists());
    }

    #[test]
    fn filename_uses_the_cdn_extension() {
        let photo = Photo {
            hash: "c".repeat(40),
            url: "https://i.guim.co.uk/img/media/x/0_0_1_1/master/10.png?width=1020".to_string(),
            width: 1020,
            location: String::new(),
            description: String::new(),
        };
        assert_eq!(photo_filename(&photo), format!("{}.png", photo.hash));
    }
}
