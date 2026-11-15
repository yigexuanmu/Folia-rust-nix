// Audio source fallback: when a platform withholds the full track or serves only a preview, ask the
// third-party sources in the configured order for a playable URL.
//
// Ported from the fork's `songUnlockService.ts`, with the same three sources and behaviour:
//   - NetEase: a third-party aggregate API, looked up by the NetEase song id.
//   - Bodian: Kuwo's bd-api; finds the same song by keyword, then asks for the 320 kbps URL after
//     reporting an ad view.
//   - Kuwo: the mobile API; the same song's rid, signed with DES, for the 128 kbps URL.
//
// This module only returns URLs. Whether to use one, and how it enters the cache and quality
// tiers, is the caller's decision.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use folia_core::Track;
use folia_store::{UnlockSettings, UnlockSource};
use md5::{Digest, Md5};
use serde_json::Value;

mod des;

/// Give up on one source after this long; a stalled source must not hold up the rest.
const SOURCE_TIMEOUT: Duration = Duration::from_secs(12);
/// Separates title and artist in the search keyword.
const KEYWORD_SEPARATOR: &str = " - ";
/// Fixed key and package name of Kuwo's mobile API, from the fork.
const KUWO_DES_KEY: &[u8; 8] = b"ylzsxkwm";
const KUWO_PACKAGE: &str = "kwplayer_ar_5.1.0.0_B_jiakong_vh.apk";
/// Bodian needs an ad-view report before it hands out a URL; the fork's parameters and signature are fixed.
const BODIAN_ADVERT: &str =
    "/api/service/advert/watch?uid=-1&token=&timestamp=1724306124436&sign=15a676d66285117ad714e8c8371691da";
const BODIAN_AUDIO: &str = "/api/play/music/v2/audioUrl";

/// Endpoints for the three sources; tests point them at a local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The third-party aggregate API for NetEase.
    pub netease: String,
    /// Kuwo search, shared by the Bodian and Kuwo sources.
    pub kuwo_search: String,
    /// Kuwo's bd-api, used by Bodian.
    pub bodian: String,
    /// Kuwo's mobile API.
    pub kuwo_mobile: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            netease: "https://music-api.gdstudio.xyz/api.php".to_owned(),
            kuwo_search: "http://search.kuwo.cn/r.s".to_owned(),
            bodian: "http://bd-api.kuwo.cn".to_owned(),
            kuwo_mobile: "http://mobi.kuwo.cn/mobi.s".to_owned(),
        }
    }
}

/// A full-track URL returned by one of the fallback sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unlocked {
    pub url: String,
    pub source: UnlockSource,
}

pub struct Unlock {
    http: reqwest::Client,
    endpoints: Endpoints,
}

impl Unlock {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http, endpoints: Endpoints::default() }
    }

    /// Overrides the endpoints so tests can use a local server.
    pub fn with_endpoints(http: reqwest::Client, endpoints: Endpoints) -> Self {
        Self { http, endpoints }
    }

    /// Asks each enabled source in the configured order and takes the first full-track URL; `None` when none answer.
    pub async fn source(&self, track: &Track, settings: &UnlockSettings) -> Option<Unlocked> {
        let order = settings.order();
        if order.is_empty() {
            return None;
        }
        let wanted = keyword(track);
        // Both Kuwo-backed sources need the same song's rid; one lookup is enough, including a miss.
        let mut matched: Option<Option<String>> = None;
        for source in order {
            let found = match source {
                UnlockSource::Netease => self.netease(track).await,
                UnlockSource::Bodian | UnlockSource::Kuwo => {
                    if matched.is_none() {
                        matched = Some(self.kuwo_id(track, &wanted).await);
                    }
                    match matched.clone().flatten() {
                        Some(id) if source == UnlockSource::Bodian => self.bodian(&id).await,
                        Some(id) => self.kuwo(&id).await,
                        None => None,
                    }
                }
            };
            match found {
                Some(unlocked) => {
                    tracing::info!(
                        source = unlocked.source.id(),
                        song = %wanted,
                        "song unlocked through an alternate source"
                    );
                    return Some(unlocked);
                }
                None => tracing::debug!(source = source.id(), song = %wanted, "alternate source had no address"),
            }
        }
        tracing::debug!(song = %wanted, "no alternate source had the whole song");
        None
    }

    /// The aggregate API indexes NetEase's catalogue: another platform's id resolves to a
    /// different song, so only NetEase tracks are asked.
    async fn netease(&self, track: &Track) -> Option<Unlocked> {
        let song = track.online.as_ref().filter(|song| song.provider == "netease")?;
        let address = with_query(&self.endpoints.netease, &[("types", "url"), ("id", song.id.as_str())])?;
        let answer = self.http.get(address).timeout(SOURCE_TIMEOUT).send().await.ok()?.json::<Value>().await.ok()?;
        let url = answer.get("url").and_then(Value::as_str).filter(|url| !url.is_empty())?;
        Some(Unlocked { url: url.to_owned(), source: UnlockSource::Netease })
    }

    /// Bodian's 320 kbps mp3, signed by bd-api.
    async fn bodian(&self, music_id: &str) -> Option<Unlocked> {
        let query = format!("br=320kmp3&musicId={music_id}&timestamp={}", millis());
        let sign = kuwotest_sign(&query, BODIAN_AUDIO);
        // The fork ignores the ad-view response too: without a URL this source has no answer.
        let advert = self
            .http
            .post(format!("{}{BODIAN_ADVERT}", self.endpoints.bodian))
            .headers(bodian_headers())
            .header(reqwest::header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(r#"{"type":5,"subType":5,"musicId":0,"adToken":""}"#)
            .timeout(SOURCE_TIMEOUT)
            .send()
            .await;
        drop(advert);
        let answer = self
            .http
            .get(format!("{}{BODIAN_AUDIO}?{query}&sign={sign}", self.endpoints.bodian))
            .headers(bodian_headers())
            .timeout(SOURCE_TIMEOUT)
            .send()
            .await
            .ok()?
            .json::<Value>()
            .await
            .ok()?;
        let url = answer.pointer("/data/audioUrl").and_then(Value::as_str).filter(|url| !url.is_empty())?;
        Some(Unlocked { url: url.to_owned(), source: UnlockSource::Bodian })
    }

    /// Kuwo's 128 kbps mp3 from the mobile API, with the query DES-encrypted into `q`.
    async fn kuwo(&self, music_id: &str) -> Option<Unlocked> {
        let query = format!("corp=kuwo&source={KUWO_PACKAGE}&p2p=1&type=convert_url2&sig=0&format=mp3&rid={music_id}");
        let sealed = kw_query(&query);
        let address = with_query(&self.endpoints.kuwo_mobile, &[("f", "kuwo"), ("q", sealed.as_str())])?;
        let answer = self
            .http
            .get(address)
            .header(reqwest::header::USER_AGENT, "okhttp/3.10.0")
            .timeout(SOURCE_TIMEOUT)
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        Some(Unlocked { url: first_url(&answer)?, source: UnlockSource::Kuwo })
    }

    /// Finds the same song in Kuwo's search results and returns its rid.
    async fn kuwo_id(&self, track: &Track, keyword: &str) -> Option<String> {
        let address = with_query(
            &self.endpoints.kuwo_search,
            &[
                ("correct", "1"),
                ("vipver", "1"),
                ("stype", "comprehensive"),
                ("encoding", "utf8"),
                ("rformat", "json"),
                ("mobi", "1"),
                ("show_copyright_off", "1"),
                ("searchapi", "6"),
                ("all", keyword),
            ],
        )?;
        let answer = self.http.get(address).timeout(SOURCE_TIMEOUT).send().await.ok()?.text().await.ok()?;
        let answer: Value = serde_json::from_str(&answer).ok()?;
        let songs = answer.pointer("/content/1/musicpage/abslist")?.as_array()?;
        let artist = track.artist.as_deref();
        songs.iter().find_map(|song| {
            let name = song.get("SONGNAME").and_then(Value::as_str)?;
            let found_artist = song.get("ARTIST").and_then(Value::as_str);
            if !matches_song(name, found_artist, &track.title, artist) {
                return None;
            }
            let rid = song.get("MUSICRID").and_then(Value::as_str)?;
            let rid = rid.rsplit('_').next().unwrap_or(rid);
            (!rid.is_empty()).then(|| rid.to_owned())
        })
    }
}

/// Search keyword: `title - artist`, or just the title when the artist is unknown.
pub fn keyword(track: &Track) -> String {
    match &track.artist {
        Some(artist) if !artist.trim().is_empty() => format!("{}{KEYWORD_SEPARATOR}{artist}", track.title),
        _ => track.title.clone(),
    }
}

/// Whether a search hit is the same song: titles must contain each other after parenthetical
/// notes are dropped, and so must artists when both are known.
pub fn matches_song(
    found_name: &str,
    found_artist: Option<&str>,
    wanted_name: &str,
    wanted_artist: Option<&str>,
) -> bool {
    let found = bare_name(found_name);
    if found.is_empty() {
        return false;
    }
    let wanted = bare_name(wanted_name);
    if !wanted.is_empty() && !found.contains(&wanted) && !wanted.contains(&found) {
        return false;
    }
    match (found_artist, wanted_artist) {
        (Some(found_artist), Some(wanted_artist)) => {
            let found = bare_artist(found_artist);
            let wanted = bare_artist(wanted_artist);
            found.is_empty() || wanted.is_empty() || found.contains(&wanted) || wanted.contains(&found)
        }
        _ => true,
    }
}

/// Lower-cased title without `(…)` / `（…）` notes; an unclosed parenthesis stays verbatim.
fn bare_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name.chars().flat_map(char::to_lowercase).peekable();
    while let Some(ch) = rest.next() {
        if ch != '(' && ch != '（' {
            out.push(ch);
            continue;
        }
        match rest.clone().position(|next| next == ')' || next == '）') {
            // Drop the note up to the first closing parenthesis, brackets included.
            Some(index) => rest.by_ref().take(index + 1).for_each(drop),
            None => out.push(ch),
        }
    }
    out.trim().to_owned()
}

/// Lower-cased artist, with `& / 、 ， , ; ；` as separators and whitespace collapsed to one space.
fn bare_artist(artist: &str) -> String {
    let spaced: String = artist
        .chars()
        .flat_map(char::to_lowercase)
        .map(|ch| if "&/、，,;；".contains(ch) { ' ' } else { ch })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Signs a Kuwo mobile query: Kuwo's DES variant (see `des.rs`) over zero-padded blocks with a
/// fixed key, then base64. An exact multiple of eight bytes gets a whole extra block.
pub fn kw_query(query: &str) -> String {
    let mut bytes = query.as_bytes().to_vec();
    let remainder = bytes.len() % 8;
    bytes.resize(bytes.len() + if remainder == 0 { 8 } else { 8 - remainder }, 0);
    let key = u64::from_le_bytes(*KUWO_DES_KEY);
    for chunk in bytes.chunks_mut(8) {
        let block = u64::from_le_bytes(<[u8; 8]>::try_from(&*chunk).unwrap_or_default());
        chunk.copy_from_slice(&des::encrypt_block(key, block).to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(&bytes)
}

/// bd-api signature: keep the query's alphanumerics, sort them, prefix `kuwotest`, append the path, take MD5.
fn kuwotest_sign(query: &str, path: &str) -> String {
    let mut chars: Vec<char> = query.chars().filter(char::is_ascii_alphanumeric).collect();
    chars.sort_unstable();
    md5_hex(&format!("kuwotest{}{path}", chars.into_iter().collect::<String>()))
}

/// Mobile headers bd-api expects; the device id changes each launch.
fn bodian_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("user-agent", "Dart/2.19 (dart:io)"),
        ("host", "bd-api.kuwo.cn"),
        ("plat", "ar"),
        ("channel", "aliopen"),
        ("ver", "3.9.0"),
        ("qimei36", "1e9970cbcdc20a031dee9f37100017e1840e"),
        ("x-forwarded-for", "1.0.1.114"),
    ] {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            headers.insert(name, value);
        }
    }
    if let Ok(device) = HeaderValue::try_from(device_id()) {
        headers.insert(HeaderName::from_static("devid"), device);
    }
    headers
}

/// `reqwest`'s `.query()` needs a separate feature, so build the query into the URL here.
fn with_query(base: &str, params: &[(&str, &str)]) -> Option<reqwest::Url> {
    reqwest::Url::parse_with_params(base, params).ok()
}

/// First URL in the body: Kuwo's mobile API wraps it in a text response.
fn first_url(text: &str) -> Option<String> {
    let start = text.find("http")?;
    let url: String = text[start..].chars().take_while(|ch| !ch.is_whitespace() && *ch != '$' && *ch != '"').collect();
    (!url.is_empty()).then_some(url)
}

fn millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn md5_hex(text: &str) -> String {
    Md5::digest(text.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A fresh device id per launch, a random digit string as in the fork.
fn device_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(millis());
    format!("{}", hasher.finish() % 100_000_000_000)
}

/// Extension taken from the URL, defaulting to mp3: the downloaded file name needs it.
pub fn audio_extension(url: &str) -> String {
    const KNOWN: [&str; 7] = ["mp3", "m4a", "flac", "aac", "wav", "ogg", "opus"];
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    if KNOWN.contains(&extension.as_str()) { extension } else { "mp3".to_owned() }
}
