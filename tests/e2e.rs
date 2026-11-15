//! End-to-end tests: a real HTTP server serves a Guardian-shaped feed and
//! gallery pages, a fake applier script records what it was asked to show,
//! and the real binary runs against both. No network, no compositor.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use chrono::{TimeDelta, Utc};
use fs2::FileExt;
use tempfile::TempDir;

const HASH_A1: &str = "a111111111111111111111111111111111111111";
const HASH_A2: &str = "a222222222222222222222222222222222222222";
const HASH_B1: &str = "b111111111111111111111111111111111111111";
const HASH_NARROW: &str = "c111111111111111111111111111111111111111";
const HASH_OLD: &str = "d111111111111111111111111111111111111111";

struct Server {
    base: String,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let content = Arc::new(Content::new(&base));
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let mut buffer = [0u8; 8192];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let target = request.split_whitespace().nth(1).unwrap_or("/");
                let path = target.split('?').next().unwrap_or("/");
                let (status, body) = match content.route(path, target) {
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", b"not found".to_vec()),
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self { base }
    }
}

struct Content {
    rss: String,
    gallery_a: String,
    gallery_b: String,
    gallery_old: String,
}

impl Content {
    fn new(base: &str) -> Self {
        let now = Utc::now();
        let rss = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
<title>Photos of the day</title><link>{base}/series/x</link><description>test</description>
<item><title>Photos of the day: test A</title><link>{base}/gallery/a</link><pubDate>{a}</pubDate></item>
<item><title>Photos of the weekend: test B</title><link>{base}/gallery/b</link><pubDate>{b}</pubDate></item>
<item><title>Photos of the day: old</title><link>{base}/gallery/old</link><pubDate>{old}</pubDate></item>
<item><title>Not a gallery at all</title><link>{base}/gallery/a</link><pubDate>{a}</pubDate></item>
</channel></rss>"#,
            a = now.to_rfc2822(),
            b = now.to_rfc2822(),
            old = (now - TimeDelta::days(10)).to_rfc2822(),
        );

        let gallery_a = page(&[
            figure(
                base,
                HASH_A1,
                1020,
                "Tokyo, Japan",
                "A man uses a laptop",
                "",
            ),
            figure(
                base,
                HASH_A2,
                1020,
                "Dublin, Ireland",
                "Bare description",
                "",
            ),
            figure(base, HASH_NARROW, 465, "Narrow", "Too small", ""),
        ]);
        let gallery_b = page(&[
            figure(base, HASH_A1, 1200, "", "", ""),
            figure(
                base,
                HASH_A2,
                1400,
                "Crop",
                "Overlay variant",
                "&crop=overlay",
            ),
            figure(base, HASH_B1, 940, "", "Bare description", ""),
        ]);
        let gallery_old = page(&[figure(base, HASH_OLD, 1020, "Old", "Old gallery", "")]);

        Self {
            rss,
            gallery_a,
            gallery_b,
            gallery_old,
        }
    }

    fn route(&self, path: &str, target: &str) -> Option<Vec<u8>> {
        match path {
            "/series/x/rss" => Some(self.rss.clone().into_bytes()),
            "/gallery/a" => Some(self.gallery_a.clone().into_bytes()),
            "/gallery/b" => Some(self.gallery_b.clone().into_bytes()),
            "/gallery/old" => Some(self.gallery_old.clone().into_bytes()),
            path if path.starts_with("/media/") => Some(format!("jpeg at {target}").into_bytes()),
            _ => None,
        }
    }
}

fn page(figures: &[String]) -> String {
    format!("<html><body>{}</body></html>", figures.join("\n"))
}

fn figure(
    base: &str,
    hash: &str,
    width: u32,
    location: &str,
    description: &str,
    extra: &str,
) -> String {
    format!(
        r##"<figure><picture>
<source srcset="{base}/media/{hash}/0_0_1_1/master/1.jpg?width={width}{extra}">
<img src="{base}/media/{hash}/0_0_1_1/master/1.jpg?width=465">
</picture>
<figcaption><h2>{location}</h2>{description}<small>Photograph: Someone/AFP</small>
<div><gu-island name="ShareButton"><a href="#">Share<svg><path d="M0 0"></path></svg></a></gu-island></div>
</figcaption></figure>"##
    )
}

struct Fixture {
    _temp: TempDir,
    dir: PathBuf,
    config: PathBuf,
    applier_log: PathBuf,
    pool: PathBuf,
}

impl Fixture {
    fn new(server: &Server) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        let pool = dir.join("pool");
        let applier_log = dir.join("applied.log");
        let script = dir.join("apply.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\n",
                applier_log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let config = dir.join("config.toml");
        fs::write(
            &config,
            format!(
                r#"
[pool]
dir = "{pool}"
retention_days = 5

[source]
series = ["{base}/series/x"]
days = 5
min_width = 600
attempts = 1
media_hosts = ["{host}"]

[apply]
command = "{script}"
args = ["{{file}}"]
ensure_daemon = false
"#,
                pool = pool.display(),
                base = server.base,
                host = server.base.trim_start_matches("http://"),
                script = script.display(),
            ),
        )
        .unwrap();

        Self {
            _temp: temp,
            dir,
            config,
            applier_log,
            pool,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_wallpaper-guardian"))
            .arg("--config")
            .arg(&self.config)
            .args(args)
            .output()
            .unwrap()
    }

    fn applied(&self) -> Vec<String> {
        fs::read_to_string(&self.applier_log)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    fn photos(&self) -> Vec<PathBuf> {
        let mut photos: Vec<PathBuf> = fs::read_dir(&self.pool)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension == "jpg")
            })
            .collect();
        photos.sort();
        photos
    }

    fn caption(&self) -> String {
        fs::read_to_string(self.pool.join("caption.txt"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn current(&self) -> PathBuf {
        PathBuf::from(
            fs::read_to_string(self.pool.join("current"))
                .unwrap()
                .trim(),
        )
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn expected_caption(hash: &str) -> &'static str {
    match hash {
        HASH_A1 => "Tokyo, Japan: A man uses a laptop",
        HASH_A2 => "Dublin, Ireland: Bare description",
        HASH_B1 => "Bare description",
        _ => panic!("unknown hash {hash}"),
    }
}

#[test]
fn fetch_dedupes_downloads_and_prunes() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    fs::create_dir_all(&fixture.pool).unwrap();
    let stale = fixture.pool.join("old.jpg");
    fs::write(&stale, b"stale").unwrap();
    fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(6 * 86_400))
        .unwrap();

    let output = fixture.run(&["fetch"]);
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("3 new photos"), "{stderr}");
    assert!(stderr.contains("1 pruned"), "{stderr}");

    let names: Vec<String> = fixture
        .photos()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            format!("{HASH_A1}.jpg"),
            format!("{HASH_A2}.jpg"),
            format!("{HASH_B1}.jpg"),
        ]
    );
    assert!(!stale.exists(), "stale photo should have been pruned");

    let captions = fs::read_to_string(fixture.pool.join("captions.tsv")).unwrap();
    assert_eq!(captions.lines().count(), 3, "{captions}");
    assert!(
        captions.contains(&format!("{HASH_A1}\tTokyo, Japan\tA man uses a laptop")),
        "{captions}"
    );
    // The duplicate in gallery B was wider, so the widest URL must be on
    // disk; the overlay crop must not win.
    let a1 = fs::read_to_string(fixture.pool.join(format!("{HASH_A1}.jpg"))).unwrap();
    assert!(a1.contains("width=1200"), "{a1}");
    let a2 = fs::read_to_string(fixture.pool.join(format!("{HASH_A2}.jpg"))).unwrap();
    assert!(a2.contains("width=1020"), "{a2}");
    assert!(!a2.contains("overlay"), "{a2}");
}

#[test]
fn next_fetches_an_empty_pool_and_writes_the_caption() {
    let server = Server::start();
    let fixture = Fixture::new(&server);

    let output = fixture.run(&["next"]);
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pool empty, fetching"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let applied = fixture.applied();
    assert_eq!(applied.len(), 1, "{applied:?}");
    let file = PathBuf::from(&applied[0]);
    assert!(
        file.starts_with(fixture.pool.canonicalize().unwrap()),
        "{}",
        file.display()
    );
    assert_eq!(fixture.current(), file);
    let hash = file.file_stem().unwrap().to_string_lossy();
    assert_eq!(fixture.caption(), expected_caption(&hash));
}

#[test]
fn deck_shows_every_photo_once_before_repeating() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    assert_success(&fixture.run(&["fetch"]));

    for _ in 0..3 {
        assert_success(&fixture.run(&["next"]));
    }
    let applied = fixture.applied();
    assert_eq!(applied.len(), 3, "{applied:?}");
    let mut distinct: Vec<&String> = applied.iter().collect();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "cycle repeated a photo: {applied:?}");

    let last = applied[2].clone();
    assert_success(&fixture.run(&["next"]));
    let applied = fixture.applied();
    assert_eq!(applied.len(), 4);
    assert_ne!(applied[3], last, "new cycle repeated the current photo");
    assert!(
        fixture
            .photos()
            .iter()
            .any(|photo| { photo.as_os_str().to_string_lossy() == applied[3].as_str() })
    );
}

#[test]
fn random_mode_avoids_an_immediate_repeat() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    fs::write(
        &fixture.config,
        fs::read_to_string(&fixture.config)
            .unwrap()
            .replace("[apply]", "[select]\nmode = \"random\"\n\n[apply]"),
    )
    .unwrap();
    assert_success(&fixture.run(&["fetch"]));
    assert_success(&fixture.run(&["next"]));
    assert_success(&fixture.run(&["next"]));
    let applied = fixture.applied();
    assert_eq!(applied.len(), 2);
    assert_ne!(applied[0], applied[1]);
}

#[test]
fn apply_sets_current_and_caption_and_missing_file_fails() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    assert_success(&fixture.run(&["fetch"]));
    let photo = fixture
        .photos()
        .into_iter()
        .find(|path| path.file_stem().unwrap() == HASH_A1)
        .unwrap();

    let output = fixture.run(&["apply", photo.to_str().unwrap()]);
    assert_success(&output);
    assert_eq!(fixture.current(), photo.canonicalize().unwrap());
    assert_eq!(fixture.caption(), expected_caption(HASH_A1));
    assert_eq!(
        fixture.applied(),
        vec![photo.canonicalize().unwrap().display().to_string()]
    );

    let output = fixture.run(&["apply", fixture.dir.join("missing.jpg").to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot find"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn status_reports_the_pool() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    assert_success(&fixture.run(&["fetch"]));
    let output = fixture.run(&["status"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pool: 3 photos"), "{stdout}");
    assert!(stdout.contains("current: none"), "{stdout}");
    assert!(stdout.contains("used: 0/3 this cycle"), "{stdout}");
}

#[test]
fn config_command_prints_effective_defaults() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    let output = fixture.run(&["config"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("[pool]"), "{stdout}");
    assert!(stdout.contains("mode = \"deck\""), "{stdout}");
    assert!(
        stdout.contains(&fixture.pool.display().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains("retention_days = 5"), "{stdout}");
}

#[test]
fn lock_skips_a_second_run() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    fs::create_dir_all(&fixture.pool).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(fixture.pool.join(".lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();

    let output = fixture.run(&["fetch"]);
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("another run is in progress"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.photos().is_empty());
}

#[test]
fn fetch_failure_leaves_the_pool_alone() {
    let server = Server::start();
    let fixture = Fixture::new(&server);
    fs::write(
        &fixture.config,
        fs::read_to_string(&fixture.config)
            .unwrap()
            .replace("/series/x", "/broken"),
    )
    .unwrap();
    fs::create_dir_all(&fixture.pool).unwrap();
    let kept = fixture.pool.join(format!("{HASH_A1}.jpg"));
    fs::write(&kept, b"kept").unwrap();
    let stale = fixture.pool.join("old.jpg");
    fs::write(&stale, b"stale").unwrap();
    fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(6 * 86_400))
        .unwrap();

    let output = fixture.run(&["fetch"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pool left untouched"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(kept.exists());
    assert!(stale.exists(), "a failed fetch must not prune the pool");
}
