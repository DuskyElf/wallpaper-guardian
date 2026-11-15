use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use ego_tree::NodeRef;
use regex::Regex;
use scraper::node::{Element, Node};
use scraper::{ElementRef, Html, Selector};

use crate::config::SourceConfig;
use crate::http::{Http, Log};

static HASH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/media/([a-f0-9]{40})/").unwrap());
static WIDTH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[?&]width=(\d+)").unwrap());
static CHROME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)view image in fullscreen|share\s*$").unwrap());

/// One downloadable photo. `hash` is the media hash from the CDN URL, which
/// also names the pool file; `width` is the CDN query width, not the source
/// image width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    pub hash: String,
    pub url: String,
    pub width: u32,
    pub location: String,
    pub description: String,
}

pub struct Source {
    config: SourceConfig,
    http: Http,
    log: Log,
    title: Regex,
}

impl Source {
    pub fn new(config: &SourceConfig, http: Http, log: Log) -> Result<Self> {
        let title = Regex::new(&config.title_pattern)
            .context("source.title_pattern is not a valid regex")?;
        Ok(Self {
            config: config.clone(),
            http,
            log,
            title,
        })
    }

    /// Read every configured series feed, take galleries inside the window,
    /// fetch each page and merge their photos. A feed that fails is fatal
    /// when no other feed produced items; a gallery page that fails is
    /// skipped.
    pub fn fetch(&self) -> Result<Vec<Photo>> {
        let cutoff_ms = Utc::now().timestamp_millis()
            - i64::try_from(self.config.days)
                .unwrap_or(i64::MAX)
                .saturating_mul(86_400_000);

        let mut links = Vec::new();
        let mut seen_links = HashSet::new();
        let mut failures = Vec::new();
        let mut feeds_read = 0usize;

        for series in &self.config.series {
            let feed_url = format!("{}/rss", series.trim_end_matches('/'));
            match self.http.text(&feed_url) {
                Ok(body) => match feed_rs::parser::parse(body.as_bytes()) {
                    Ok(feed) => {
                        feeds_read += 1;
                        for item in feed.entries {
                            let title = item
                                .title
                                .as_ref()
                                .map(|title| title.content.as_str())
                                .unwrap_or_default();
                            if !self.title.is_match(title) {
                                continue;
                            }
                            let Some(published) = item.published else {
                                continue;
                            };
                            if published.timestamp_millis() < cutoff_ms {
                                continue;
                            }
                            let Some(link) = item.links.first() else {
                                continue;
                            };
                            if seen_links.insert(link.href.clone()) {
                                links.push(link.href.clone());
                            }
                        }
                    }
                    Err(error) => failures.push(format!("{feed_url}: {error}")),
                },
                Err(error) => failures.push(format!("{feed_url}: {error:#}")),
            }
        }

        if feeds_read == 0 {
            bail!("RSS fetch failed: {}", failures.join("; "));
        }
        for failure in &failures {
            self.log.info(format!("skipping feed: {failure}"));
        }
        self.log.debug(format!(
            "{} galleries in the {}-day window",
            links.len(),
            self.config.days
        ));

        let mut variants: HashMap<String, Photo> = HashMap::new();
        let mut captions: HashMap<String, (String, String)> = HashMap::new();

        for link in links {
            let html = match self.http.text(&link) {
                Ok(html) => html,
                Err(first) => match self.http.text(&format!("{link}.json")) {
                    Ok(html) => html,
                    Err(second) => {
                        self.log
                            .info(format!("skipping gallery {link}: {first:#}; {second:#}"));
                        continue;
                    }
                },
            };
            let parsed = parse_gallery_page(&html, &self.config.media_hosts);
            self.log.debug(format!("{} photos in {link}", parsed.len()));
            for photo in parsed {
                match variants.get_mut(&photo.hash) {
                    Some(current) if photo.width > current.width => {
                        current.url = photo.url;
                        current.width = photo.width;
                    }
                    Some(_) => {}
                    None => {
                        variants.insert(photo.hash.clone(), photo.clone());
                    }
                }
                if !captions.contains_key(&photo.hash)
                    && (!photo.location.is_empty() || !photo.description.is_empty())
                {
                    captions.insert(photo.hash.clone(), (photo.location, photo.description));
                }
            }
        }

        let mut photos: Vec<Photo> = variants
            .into_values()
            .filter(|photo| photo.width >= self.config.min_width)
            .map(|mut photo| {
                if let Some((location, description)) = captions.remove(&photo.hash) {
                    photo.location = location;
                    photo.description = description;
                }
                photo
            })
            .collect();
        photos.sort_by(|left, right| left.hash.cmp(&right.hash));
        Ok(photos)
    }
}

/// Parse one gallery page. Pure: no network, no disk. A figure only counts
/// when it has a caption and at least one usable CDN variant.
pub fn parse_gallery_page(html: &str, media_hosts: &[String]) -> Vec<Photo> {
    let document = Html::parse_document(html);
    let figure = Selector::parse("figure").unwrap();
    let caption = Selector::parse("figcaption").unwrap();
    let heading = Selector::parse("h1, h2, h3").unwrap();

    let mut photos = Vec::new();
    for figure in document.select(&figure) {
        let Some(caption) = figure.select(&caption).next() else {
            continue;
        };
        let Some((hash, url, width)) = pick_variant(&figure, media_hosts) else {
            continue;
        };
        let location = caption
            .select(&heading)
            .next()
            .map(|heading| collapse(&heading.text().collect::<String>()))
            .unwrap_or_default();
        let description = caption_description(caption);
        photos.push(Photo {
            hash,
            url,
            width,
            location,
            description,
        });
    }
    photos
}

/// Widest CDN variant in the figure, preferring one without an explicit
/// height when widths tie. Variants with overlay/precrop crops are skipped.
fn pick_variant(figure: &ElementRef<'_>, media_hosts: &[String]) -> Option<(String, String, u32)> {
    let media = Selector::parse("img, source").unwrap();
    let mut best: Option<(String, String, u32, bool)> = None;

    for element in figure.select(&media) {
        let mut urls = Vec::new();
        if let Some(src) = element.value().attr("src") {
            urls.push(src.to_string());
        }
        if let Some(srcset) = element.value().attr("srcset") {
            for candidate in srcset.split(',') {
                if let Some(url) = candidate.split_whitespace().next() {
                    urls.push(url.to_string());
                }
            }
        }

        for url in urls {
            let host_ok =
                media_hosts.is_empty() || media_hosts.iter().any(|host| url.contains(host));
            if !host_ok || url.contains("overlay") || url.contains("precrop") {
                continue;
            }
            let Some(hash) = HASH_RE
                .captures(&url)
                .map(|captures| captures[1].to_string())
            else {
                continue;
            };
            let width = WIDTH_RE
                .captures(&url)
                .and_then(|captures| captures[1].parse().ok())
                .unwrap_or(0);
            let no_height = !url.contains("height=");
            let better = match &best {
                None => true,
                Some((_, _, best_width, best_no_height)) => {
                    width > *best_width || (width == *best_width && no_height && !best_no_height)
                }
            };
            if better {
                best = Some((hash, url, width, no_height));
            }
        }
    }

    best.map(|(hash, url, width, _)| (hash, url, width))
}

/// All descendant text of the caption minus location headings, credit,
/// share widgets and images, with whitespace collapsed.
fn caption_description(caption: ElementRef<'_>) -> String {
    let mut text = String::new();
    collect_text(*caption, &mut text);
    let text = CHROME_RE.replace_all(&text, " ");
    collapse(&text)
}

fn collect_text(node: NodeRef<'_, Node>, out: &mut String) {
    for child in node.children() {
        match child.value() {
            Node::Text(text) => out.push_str(&text.text),
            Node::Element(element) => {
                if skip_caption_element(element) {
                    continue;
                }
                collect_text(child, out);
            }
            _ => {}
        }
    }
}

fn skip_caption_element(element: &Element) -> bool {
    matches!(
        element.name(),
        "h1" | "h2" | "h3" | "small" | "gu-island" | "button" | "svg" | "picture"
    ) || element
        .attr("class")
        .is_some_and(|class| class.to_ascii_lowercase().contains("share"))
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"
        <html><body>
        <figure><picture><img src="https://i.guim.co.uk/img/media/hero/0_0_1_1/master/1.jpg?width=1020"></picture></figure>
        <figure><picture>
          <source srcset="https://i.guim.co.uk/img/media/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/0_0_6317_4211/master/6317.jpg?width=700&amp;dpr=1&amp;s=none&amp;crop=none">
          <source srcset="https://i.guim.co.uk/img/media/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/0_0_6317_4211/master/6317.jpg?width=1020&amp;dpr=1&amp;s=none&amp;crop=none">
          <img src="https://i.guim.co.uk/img/media/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/0_0_6317_4211/master/6317.jpg?width=465">
        </picture>
        <figcaption>
          <h2>Tokyo, Japan</h2>
          A man uses a laptop after efforts to repair a major outage
          <small>Photograph: Someone/AFP/Getty Images</small>
          <div><gu-island name="ShareButton"><a href="#">Share<svg><path d="M0 0"></path></svg></a></gu-island></div>
        </figcaption></figure>
        <figure><picture>
          <img src="https://i.guim.co.uk/img/media/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/0_0_1_1/master/1.jpg?width=940&amp;height=600">
          <img src="https://i.guim.co.uk/img/media/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/0_0_1_1/master/1.jpg?width=940">
        </picture>
        <figcaption><h2>Dublin, Ireland</h2>Bare description<small>Photograph: X</small></figcaption></figure>
        <figure><picture>
          <img src="https://i.guim.co.uk/img/media/cccccccccccccccccccccccccccccccccccccccc/0_0_1_1/master/1.jpg?width=1200&amp;crop=overlay">
        </picture>
        <figcaption><h2>Crop</h2>Overlay variant</figcaption></figure>
        <figure><picture>
          <img src="https://example.test/img/media/dddddddddddddddddddddddddddddddddddddddd/0_0_1_1/master/1.jpg?width=1200">
        </picture>
        <figcaption><h2>Elsewhere</h2>Wrong host</figcaption></figure>
        </body></html>
    "##;

    fn hosts() -> Vec<String> {
        vec!["i.guim.co.uk".to_string()]
    }

    #[test]
    fn parses_photos_and_captions() {
        let photos = parse_gallery_page(PAGE, &hosts());
        assert_eq!(photos.len(), 2, "{photos:#?}");

        let tokyo = &photos[0];
        assert_eq!(tokyo.width, 1020);
        assert!(tokyo.url.contains("width=1020"));
        assert_eq!(tokyo.location, "Tokyo, Japan");
        assert_eq!(
            tokyo.description,
            "A man uses a laptop after efforts to repair a major outage"
        );

        let dublin = &photos[1];
        assert_eq!(dublin.width, 940);
        assert!(!dublin.url.contains("height="));
        assert_eq!(dublin.description, "Bare description");
    }

    #[test]
    fn empty_media_hosts_accepts_any_host() {
        let photos = parse_gallery_page(PAGE, &[]);
        assert_eq!(photos.len(), 3);
        assert!(photos.iter().any(|photo| photo.location == "Elsewhere"));
    }
}
