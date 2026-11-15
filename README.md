# wallpaper-guardian

Pulls the Guardian's "photos of the day" galleries into a rolling photo pool,
then applies one photo at a time through a wallpaper command. It is a plain
CLI plus a home-manager module that generates the systemd user units.

The pool keeps photos for five days by default. `next` walks a deck: every
photo is shown once per cycle, and a new cycle never starts with the photo
that is currently on screen. A small `caption.txt` file next to the pool
carries the photo's location and description for overlays.

## Install

### home-manager (flake)

```nix
{
  inputs.wallpaper-guardian = {
    url = "github:DuskyElf/wallpaper-guardian";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  # inside a home-manager module
  imports = [ inputs.wallpaper-guardian.homeManagerModules.default ];

  services.wallpaper-guardian = {
    enable = true;
    settings = {
      source.days = 5;
      apply.args = [
        "img"
        "--transition-type"
        "fade"
        "--transition-duration"
        "2"
        "{file}"
      ];
    };
  };
}
```

The module only needs home-manager. It installs the binary, writes
`$XDG_CONFIG_HOME/wallpaper-guardian/config.toml`, and creates four systemd
user units: a fetch timer, a rotation timer, a session-start apply, and a
resume apply.

| Option | Default | Meaning |
| --- | --- | --- |
| `enable` | `false` | Install and run everything |
| `package` | flake package | Package with the `wallpaper-guardian` binary |
| `settings` | `{}` | TOML config, same keys as the file below |
| `configFile` | `null` | Use this file instead of generating one from `settings` |
| `target` | `"graphical-session.target"` | Target the services belong to |
| `waylandDisplay` | `null` | `WAYLAND_DISPLAY` for the units, inherits by default |
| `environment` | `{}` | Extra environment variables |
| `path` | `[]` | Extra PATH entries |
| `fetch.enable` | `true` | Fetch from a timer |
| `fetch.onBoot` | `"15min"` | Delay after boot before the first fetch |
| `fetch.interval` | `"12h"` | Time between fetches |
| `rotate.enable` | `true` | Rotate from a timer |
| `rotate.onCalendar` | `"*:0/6"` | Rotation schedule |
| `startup.enable` | `true` | Apply at session start |
| `startup.after` | `["awww-daemon.service"]` | Units the startup apply waits for |
| `resume.enable` | `true` | Apply after suspend |

Set `waylandDisplay = "wayland-1"` when the wallpaper daemon has to start
before the compositor exports its socket.

### Binary

```sh
cargo install --path .
# or
nix build .#default
```

The binary has no systemd or Nix dependency. Run `fetch` and `next` from any
scheduler on any Linux desktop.

## Commands

```
wallpaper-guardian [--config PATH] [-v] <command>

fetch            read the feeds, download new photos, prune expired ones
next             apply a photo not shown this cycle (fetches when the pool is empty)
apply <file>     apply a specific file and record it as current
status           print pool size, photo count, current photo and deck progress
config           print the effective configuration as TOML and exit
```

`--config` also reads `$WALLPAPER_GUARDIAN_CONFIG`. Without either, it uses
`$XDG_CONFIG_HOME/wallpaper-guardian/config.toml`, then built-in defaults. A
missing explicit file is an error; a missing default file is fine.

`fetch` exits non-zero when the feed itself cannot be read. The pool and its
photos are left untouched in that case. A gallery page that fails is skipped.

## Configuration

The file is TOML. Every key is optional; unknown keys are rejected. Example:

```toml
[pool]
dir = "~/.cache/wallpaper-guardian"
retention_days = 5

[source]
series = [ "https://www.theguardian.com/news/series/ten-best-photographs-of-the-day" ]
days = 5
min_width = 600
attempts = 2

[select]
mode = "deck"

[apply]
command = "awww"
args = [ "img", "--transition-type", "fade", "--transition-duration", "2", "{file}" ]
ensure_daemon = true

[caption]
with_location = "{location}: {description}"
without_location = "{description}"
```

### `[pool]`

| Key | Default | Meaning |
| --- | --- | --- |
| `dir` | `$XDG_CACHE_HOME/wallpaper-guardian` | Photo and state directory; `~` expands |
| `retention_days` | `5` | Delete photos older than this many days by mtime |

### `[source]`

| Key | Default | Meaning |
| --- | --- | --- |
| `series` | Guardian ten-best-photographs series | Series pages; each is read at `<series>/rss` |
| `days` | `5` | Only gallery items published within this window |
| `min_width` | `600` | Skip CDN variants narrower than this |
| `user_agent` | Chrome user agent | Sent with every request |
| `timeout_secs` | `45` | Timeout for feeds and gallery pages |
| `download_timeout_secs` | `120` | Timeout for a photo download |
| `attempts` | `2` | Attempts per request, including the first |
| `media_hosts` | `["i.guim.co.uk"]` | Hosts whose URLs count as photos; empty accepts any host |
| `title_pattern` | `(?i)photos? of the (day\|weekend)` | RSS titles worth reading |

### `[apply]`

| Key | Default | Meaning |
| --- | --- | --- |
| `command` | `"awww"` | Wallpaper command |
| `args` | `awww img --transition-type fade --transition-duration 2 {file}` | Arguments; `{file}` expands to the photo path |
| `ensure_daemon` | `true` | Probe for a live compositor and start the daemon when missing |
| `daemon_command` | `"awww-daemon"` | Daemon binary |
| `daemon_args` | `[]` | Daemon arguments |
| `query_args` | `["query"]` | Probe arguments, run against `command` |
| `daemon_wait_ms` | `300` | Delay between readiness probes |
| `daemon_tries` | `10` | Readiness probes before giving up |

`apply.command` is not tied to awww. Point it at `swww`, `hyprpaper`, or a
script and set `args` accordingly. `ensure_daemon = false` skips the probe and
daemon start entirely.

### `[select]`

| Key | Default | Meaning |
| --- | --- | --- |
| `mode` | `"deck"` | `deck` shows each photo once per cycle; `random` avoids an immediate repeat |

### `[caption]`

| Key | Default | Meaning |
| --- | --- | --- |
| `file` | `"caption.txt"` | Overlay file, relative to the pool directory unless absolute |
| `with_location` | `"{location}: {description}"` | Template when the photo has a location |
| `without_location` | `"{description}"` | Template when it does not |

Templates accept `{location}` and `{description}`. The file is rewritten on
every apply and removed when the photo has no description.

## Pool layout

```
$XDG_CACHE_HOME/wallpaper-guardian/
  <hash>.jpg        photo files, named by the CDN media hash
  captions.tsv      hash<TAB>location<TAB>description index
  caption.txt       overlay caption for the current photo
  current           full path of the current photo
  used              photos shown in this deck cycle
  .lock             advisory lock held by fetch and next
```

## Development

```sh
cargo test        # unit tests plus end-to-end tests against a local HTTP server
cargo clippy
nix build
```

The end-to-end tests serve Guardian-shaped RSS and gallery pages from a local
socket and run the real binary against a fake applier script.
