// 音源兜底（歌曲解锁）：平台不给地址、或只肯给一段试听时，按设置里的顺序去第三方音源取整首的地址。
//
// 三个音源移植自 fork 的 `songUnlockService.ts`，行为保持一致：
//   · 网易云 —— 第三方聚合 API，按网易云自己的歌曲 id 查；
//   · 波点 —— 酷我 bd-api，先用关键词在酷我搜到同一首歌，再要 320kbps 的地址（先上报一次广告观看）；
//   · 酷我 —— 酷我移动端接口，同一首歌的 rid，DES 签名后取 128kbps 的地址。
//
// 这里只负责把地址拿回来：要不要用、怎么放进缓存与音质档位，由调用方决定。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use folia_core::Track;
use folia_store::{UnlockSettings, UnlockSource};
use md5::{Digest, Md5};
use serde_json::Value;

mod des;

/// 任何一个音源最多等这么久，超过就换下一个。
const SOURCE_TIMEOUT: Duration = Duration::from_secs(12);
/// 搜索关键字里歌名与歌手之间的分隔。
const KEYWORD_SEPARATOR: &str = " - ";
/// 酷我移动端接口的固定密钥与安装包名，同 fork。
const KUWO_DES_KEY: &[u8; 8] = b"ylzsxkwm";
const KUWO_PACKAGE: &str = "kwplayer_ar_5.1.0.0_B_jiakong_vh.apk";
/// 波点取地址前要先上报一次广告观看，参数与签名都写死（同 fork 用的那一次）。
const BODIAN_ADVERT: &str =
    "/api/service/advert/watch?uid=-1&token=&timestamp=1724306124436&sign=15a676d66285117ad714e8c8371691da";
const BODIAN_AUDIO: &str = "/api/play/music/v2/audioUrl";

/// 三个音源所在的地址；测试把它们指向本地服务器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// 网易云的第三方聚合 API。
    pub netease: String,
    /// 酷我的搜索接口，波点与酷我两路共用。
    pub kuwo_search: String,
    /// 酷我 bd-api，波点音乐走这里。
    pub bodian: String,
    /// 酷我移动端接口。
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

/// 一个音源交出来的整首地址。
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

    /// 换一套地址，测试用。
    pub fn with_endpoints(http: reqwest::Client, endpoints: Endpoints) -> Self {
        Self { http, endpoints }
    }

    /// 按设置里的顺序问每个启用的音源，第一个给出整首地址的就是答案；都不给就返回 `None`。
    pub async fn source(&self, track: &Track, settings: &UnlockSettings) -> Option<Unlocked> {
        let order = settings.order();
        if order.is_empty() {
            return None;
        }
        let wanted = keyword(track);
        // 两路酷我音源都要先在酷我的目录里找到同一首歌：找到一次就够，没找到也是。
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

    /// 聚合 API 索引的是网易云的曲库：别的平台的 id 问过去只会是另一首歌，所以只有网易云的歌才问它。
    async fn netease(&self, track: &Track) -> Option<Unlocked> {
        let song = track.online.as_ref().filter(|song| song.provider == "netease")?;
        let address = with_query(&self.endpoints.netease, &[("types", "url"), ("id", song.id.as_str())])?;
        let answer = self.http.get(address).timeout(SOURCE_TIMEOUT).send().await.ok()?.json::<Value>().await.ok()?;
        let url = answer.get("url").and_then(Value::as_str).filter(|url| !url.is_empty())?;
        Some(Unlocked { url: url.to_owned(), source: UnlockSource::Netease })
    }

    /// 波点的 320kbps mp3：地址由 bd-api 签名后给出。
    async fn bodian(&self, music_id: &str) -> Option<Unlocked> {
        let query = format!("br=320kmp3&musicId={music_id}&timestamp={}", millis());
        let sign = kuwotest_sign(&query, BODIAN_AUDIO);
        // 观看广告这一步的结果 fork 也不看：拿不到地址就当这一个音源没有。
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

    /// 酷我移动端的 128kbps mp3：查询串 DES 加密后放进 `q`。
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

    /// 在酷我的搜索结果里找同一首歌，给出它的 rid。
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

/// 问名字用的关键字：`歌名 - 歌手`，没有歌手时只有歌名。
pub fn keyword(track: &Track) -> String {
    match &track.artist {
        Some(artist) if !artist.trim().is_empty() => format!("{}{KEYWORD_SEPARATOR}{artist}", track.title),
        _ => track.title.clone(),
    }
}

/// 搜索结果是不是要找的那首歌：歌名去掉括号注释后互相包含；两边都有歌手时，歌手也要互相包含。
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

/// 歌名的小写形式，去掉 `(…)` 与 `（…）` 里的注释；括号没有合上时原样留着。
fn bare_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name.chars().flat_map(char::to_lowercase).peekable();
    while let Some(ch) = rest.next() {
        if ch != '(' && ch != '（' {
            out.push(ch);
            continue;
        }
        match rest.clone().position(|next| next == ')' || next == '）') {
            // 注释到第一个右括号为止，连同括号一起丢掉。
            Some(index) => rest.by_ref().take(index + 1).for_each(drop),
            None => out.push(ch),
        }
    }
    out.trim().to_owned()
}

/// 歌手名的小写形式，`& / 、 ， , ; ；` 都算分隔，空白收成一个空格。
fn bare_artist(artist: &str) -> String {
    let spaced: String = artist
        .chars()
        .flat_map(char::to_lowercase)
        .map(|ch| if "&/、，,;；".contains(ch) { ' ' } else { ch })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 酷我移动端查询串的签名：酷我那版 DES（见 `des.rs`）逐块加密，固定密钥，补零到整块
/// （正好整块时再补一整块），再 base64。
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

/// bd-api 的签名：查询串里只留字母数字、排序，前面加 `kuwotest`，后面接路径，取 MD5。
fn kuwotest_sign(query: &str, path: &str) -> String {
    let mut chars: Vec<char> = query.chars().filter(char::is_ascii_alphanumeric).collect();
    chars.sort_unstable();
    md5_hex(&format!("kuwotest{}{path}", chars.into_iter().collect::<String>()))
}

/// bd-api 要的移动端请求头；设备号每次启动换一个。
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

/// 把查询参数拼到地址上：`reqwest` 的 `.query()` 要额外的特性开关，这里直接拼。
fn with_query(base: &str, params: &[(&str, &str)]) -> Option<reqwest::Url> {
    reqwest::Url::parse_with_params(base, params).ok()
}

/// 响应正文里第一个地址：酷我移动端把地址包在一段文本里。
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

/// 每次启动一个设备号，同 fork 的随机数字串。
fn device_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(millis());
    format!("{}", hasher.finish() % 100_000_000_000)
}

/// 地址里的文件扩展名，拿不准时按 mp3 算：下载下来的文件名要用它。
pub fn audio_extension(url: &str) -> String {
    const KNOWN: [&str; 7] = ["mp3", "m4a", "flac", "aac", "wav", "ogg", "opus"];
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    if KNOWN.contains(&extension.as_str()) { extension } else { "mp3".to_owned() }
}
