// Audio source fallback: matching and signing for the three sources, and the point where a
// fallback is only asked for when the platform cannot play.
use folia_core::{AudioQuality, OnlineId, Track};
use folia_omni::unlock::{self, Endpoints, Unlock};
use folia_omni::{
    Account, AudioSource, BoxFuture, Collection, CollectionDetail, ExploreSection, LoginCode, LoginState, Omni,
    OmniError, Page, Provider, ProviderLyrics, RadioFeed,
};
use folia_store::{UnlockServer, UnlockSettings, UnlockSource};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Outputs of the fork's `kwDESEncrypt` for these queries, pinning the ported DES variant and its
/// padding: it merges the S-boxes with P and reads whole blocks little-endian, so it differs from
/// standard DES.
#[test]
fn kuwo_signature_matches_the_fork() {
    let cases = [
        (
            "corp=kuwo&source=kwplayer_ar_5.1.0.0_B_jiakong_vh.apk&p2p=1&type=convert_url2&sig=0&format=mp3&rid=1",
            "NI8S5evAnmHH4UXcuKKslDk4RFzONrTj1BvCoyMODMTUvpQlWnft3Eccd/NC9JdLFsVy/tvCgksRUHX6eSWQFEXV3zZS3yrqb2ASAUwCJKYLJ6ShYiU19qWNmoyxI7Y+uiPVjLRovgU=",
        ),
        ("abc", "5qlmiytFsQY="),
        // An exact multiple of eight bytes gets a whole extra zero block, matching the fork's `mode === 0`.
        ("12345678", "Njw+DTAxpGxqY0iIU+Qd0Q=="),
        ("123456789", "Njw+DTAxpGwIIc44uKhZLQ=="),
    ];
    for (query, expected) in cases {
        assert_eq!(unlock::kw_query(query), expected, "{query}");
    }
}

#[test]
fn songs_match_by_bare_name_and_artist() {
    let cases: [(&str, Option<&str>, &str, Option<&str>, bool); 8] = [
        ("七里香", Some("周杰伦"), "七里香", Some("周杰伦"), true),
        // Parenthetical notes (live versions, soundtrack names) do not take part in the match.
        ("七里香 (Live)", Some("周杰伦"), "七里香", Some("周杰伦"), true),
        ("七里香（电视剧《…》主题曲）", Some("周杰伦"), "七里香", Some("周杰伦"), true),
        ("七里香", Some("周杰伦"), "七里香", Some("林俊杰"), false),
        // Mutual containment counts as the same title: search hits often append "instrumental" or an album name.
        ("七里香 伴奏", Some("周杰伦"), "七里香", Some("周杰伦"), true),
        // Artists also match by mutual containment, ignoring word order, as in the fork.
        ("因为爱情", Some("陈奕迅/王菲"), "因为爱情", Some("王菲 & 陈奕迅"), false),
        ("", Some("周杰伦"), "七里香", Some("周杰伦"), false),
        ("七里香", Some("周杰伦"), "七里香", None, true),
    ];
    for (found, found_artist, wanted, wanted_artist, expected) in cases {
        assert_eq!(
            unlock::matches_song(found, found_artist, wanted, wanted_artist),
            expected,
            "{found:?}/{found_artist:?} against {wanted:?}/{wanted_artist:?}"
        );
    }
    // An unclosed parenthesis stays verbatim, matching the fork's regex.
    assert!(unlock::matches_song("七里香 (Live", Some("周杰伦"), "七里香", Some("周杰伦")));
}

#[test]
fn keywords_and_extensions_follow_the_address() {
    let track = Track {
        title: "七里香".into(),
        artist: Some("周杰伦".into()),
        online: Some(OnlineId { provider: "netease".into(), id: "1".into() }),
        ..Default::default()
    };
    assert_eq!(unlock::keyword(&track), "七里香 - 周杰伦");
    let bare = Track { artist: None, ..track.clone() };
    assert_eq!(unlock::keyword(&bare), "七里香");
    assert_eq!(unlock::audio_extension("https://a.test/x.flac?auth=1"), "flac");
    assert_eq!(unlock::audio_extension("https://a.test/stream"), "mp3");
    assert_eq!(unlock::audio_extension("https://a.test/x.MP3#t"), "mp3");
}

/// Local server: answers by request path for each of the three sources; `/audio/` always serves audio bytes.
struct Server {
    base: String,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(self.base.trim_start_matches("http://"));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Server {
    fn start(answers: Vec<(&'static str, String)>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let stop = stop.clone();
            let base = base.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let Ok((mut socket, _)) = listener.accept() else {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    };
                    let mut head = [0_u8; 4096];
                    let read = socket.read(&mut head).unwrap_or(0);
                    let head = String::from_utf8_lossy(&head[..read]).to_string();
                    let request = head.lines().next().unwrap_or_default().to_string();
                    let payload = if request.contains("/audio/") {
                        ("audio/mpeg", vec![7_u8; 8192])
                    } else {
                        let body = answers
                            .iter()
                            .find(|(prefix, _)| request.contains(prefix))
                            .map(|(_, body)| body.replace("http://{base}", &base).replace("{base}", &base))
                            .unwrap_or_else(|| r#"{"code":404}"#.to_owned());
                        let kind = if body.starts_with('{') { "application/json" } else { "text/plain" };
                        (kind, body.into_bytes())
                    };
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload.0,
                        payload.1.len()
                    );
                    let _ = socket.write_all(header.as_bytes());
                    let _ = socket.write_all(&payload.1);
                }
            })
        };
        Self { base, stop, worker: Some(worker) }
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            netease: format!("{}/api.php", self.base),
            kuwo_search: format!("{}/r.s", self.base),
            bodian: self.base.clone(),
            kuwo_mobile: format!("{}/mobi.s", self.base),
        }
    }
}

/// Kuwo puts search hits in `content[1].musicpage.abslist`; `content[0]` is another aggregate block.
fn search_answer() -> String {
    r#"{"content":[{"type":"comprehensive"},{"musicpage":{"abslist":[
        {"MUSICRID":"MUSIC_1","SONGNAME":"别的歌","ARTIST":"别人"},
        {"MUSICRID":"MUSIC_600847","SONGNAME":"七里香 (Live)","ARTIST":"周杰伦"}
    ]}}]}"#
        .to_owned()
}

/// Enables only the given sources.
fn settings(sources: &[(UnlockSource, bool)]) -> UnlockSettings {
    let mut settings = UnlockSettings::default();
    for key in UnlockSource::ALL {
        settings.set_server(key, false);
    }
    for (key, enabled) in sources {
        settings.set_server(*key, *enabled);
    }
    settings
}

fn locked_track(provider: &str) -> Track {
    Track {
        id: 7,
        title: "七里香".into(),
        artist: Some("周杰伦".into()),
        online: Some(OnlineId { provider: provider.into(), id: "186016".into() }),
        ..Default::default()
    }
}

/// Bodian: match the song in Kuwo search first, then ask for the 320 kbps URL.
#[tokio::test]
async fn bodian_answers_with_the_matched_song() {
    let server = Server::start(vec![
        ("/r.s", search_answer()),
        ("/api/play/music/v2/audioUrl", r#"{"code":200,"data":{"audioUrl":"http://{base}/audio/bodian.mp3"}}"#.into()),
        ("/api/service/advert/watch", r#"{"code":200}"#.into()),
    ]);
    let unlock = Unlock::with_endpoints(reqwest::Client::new(), server.endpoints());
    let found = unlock.source(&locked_track("netease"), &settings(&[(UnlockSource::Bodian, true)])).await.unwrap();
    assert_eq!(found.source, UnlockSource::Bodian);
    assert_eq!(found.url, format!("{}/audio/bodian.mp3", server.base));
}

/// Kuwo: the signed query returns text with the URL embedded.
#[tokio::test]
async fn kuwo_reads_the_address_out_of_its_answer() {
    let server = Server::start(vec![
        ("/r.s", search_answer()),
        ("/mobi.s", "rid=600847&url=http://{base}/audio/kuwo.mp3$bitrate=128".into()),
    ]);
    let unlock = Unlock::with_endpoints(reqwest::Client::new(), server.endpoints());
    let found = unlock.source(&locked_track("netease"), &settings(&[(UnlockSource::Kuwo, true)])).await.unwrap();
    assert_eq!(found.source, UnlockSource::Kuwo);
    assert_eq!(found.url, format!("{}/audio/kuwo.mp3", server.base));
}

/// The aggregate API indexes NetEase only: another platform's id points at a different song, so skip this source.
#[tokio::test]
async fn the_aggregate_api_is_only_asked_for_netease_songs() {
    let server = Server::start(vec![("/api.php", r#"{"url":"http://{base}/audio/netease.mp3"}"#.into())]);
    let unlock = Unlock::with_endpoints(reqwest::Client::new(), server.endpoints());
    let settings = settings(&[(UnlockSource::Netease, true)]);
    let found = unlock.source(&locked_track("netease"), &settings).await.unwrap();
    assert_eq!(found.source, UnlockSource::Netease);
    assert_eq!(found.url, format!("{}/audio/netease.mp3", server.base));
    assert!(unlock.source(&locked_track("qq"), &settings).await.is_none());
}

/// With the master switch off, no source is asked.
#[tokio::test]
async fn the_switch_stops_every_source() {
    let server = Server::start(vec![("/api.php", r#"{"url":"http://{base}/audio/x.mp3"}"#.into())]);
    let unlock = Unlock::with_endpoints(reqwest::Client::new(), server.endpoints());
    let mut settings = settings(&[(UnlockSource::Netease, true)]);
    settings.set_enabled(false);
    assert!(unlock.source(&locked_track("netease"), &settings).await.is_none());
}

/// When the platform refuses the track, playback receives the full track from a fallback source.
#[tokio::test]
async fn a_refused_song_plays_from_the_alternate_source() {
    let server = Server::start(vec![
        ("/r.s", search_answer()),
        ("/api/play/music/v2/audioUrl", r#"{"code":200,"data":{"audioUrl":"http://{base}/audio/whole.mp3"}}"#.into()),
        ("/api/service/advert/watch", r#"{"code":200}"#.into()),
    ]);
    let temp = tempfile::tempdir().unwrap();
    let omni = Omni::with_providers(
        temp.path().into(),
        "unused".into(),
        vec![Box::new(Locked { refusal: Refusal::Subscription, preview: String::new(), calls: Default::default() })],
    )
    .unwrap()
    .with_unlock_endpoints(server.endpoints());
    omni.restore_unlock(settings(&[(UnlockSource::Bodian, true)]));
    let path = omni.fetch_track_audio(&locked_track("netease"), AudioQuality::High).await.unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 8192);
    assert!(path.to_string_lossy().ends_with("-high.mp3"), "{}", path.display());
}

/// With no fallback URL either, the platform's own failure is reported unchanged.
#[tokio::test]
async fn a_refused_song_without_an_answer_keeps_the_refusal() {
    let server = Server::start(vec![]);
    let temp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let omni = Omni::with_providers(
        temp.path().into(),
        "unused".into(),
        vec![Box::new(Locked { refusal: Refusal::Subscription, preview: String::new(), calls: calls.clone() })],
    )
    .unwrap()
    .with_unlock_endpoints(server.endpoints());
    omni.restore_unlock(settings(&[(UnlockSource::Bodian, true)]));
    let error = omni.fetch_track_audio(&locked_track("netease"), AudioQuality::High).await.unwrap_err();
    assert!(matches!(error, OmniError::SubscriptionRequired), "{error:?}");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

/// A preview survives when no fallback answers, so playback can still start.
#[tokio::test]
async fn a_preview_stays_when_no_alternate_source_answers() {
    let server = Server::start(vec![]);
    let temp = tempfile::tempdir().unwrap();
    let omni = Omni::with_providers(
        temp.path().into(),
        "unused".into(),
        vec![Box::new(Locked {
            refusal: Refusal::Preview,
            preview: format!("{}/audio/preview.mp3", server.base),
            calls: Default::default(),
        })],
    )
    .unwrap()
    .with_unlock_endpoints(server.endpoints());
    omni.restore_unlock(settings(&[(UnlockSource::Bodian, true)]));
    let input = omni.prepare_track_audio(&locked_track("netease"), AudioQuality::High).await.unwrap();
    assert!(input.path.to_string_lossy().ends_with("-preview.mp3"), "{}", input.path.display());
}

enum Refusal {
    Subscription,
    Preview,
}

struct Locked {
    refusal: Refusal,
    preview: String,
    calls: Arc<AtomicUsize>,
}

fn unsupported<'a, T: Send + 'a>() -> BoxFuture<'a, Result<T, OmniError>> {
    Box::pin(async { Err(OmniError::Unsupported("fixture")) })
}

impl Provider for Locked {
    fn id(&self) -> &'static str {
        "netease"
    }
    fn display_name(&self) -> &'static str {
        "fixture"
    }
    fn search_songs<'a>(&'a self, _: &'a str, _: u32, _: u32) -> BoxFuture<'a, Result<Page<Track>, OmniError>> {
        Box::pin(async { Ok(Page::empty(0)) })
    }
    fn audio_source<'a>(
        &'a self,
        _: &'a OnlineId,
        quality: AudioQuality,
    ) -> BoxFuture<'a, Result<AudioSource, OmniError>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        match self.refusal {
            Refusal::Subscription => Box::pin(async { Err(OmniError::SubscriptionRequired) }),
            Refusal::Preview => {
                let url = self.preview.clone();
                Box::pin(async move { Ok(AudioSource { url, extension: "mp3".into(), preview: true, level: quality }) })
            }
        }
    }
    fn lyrics<'a>(&'a self, _: &'a OnlineId) -> BoxFuture<'a, Result<Option<ProviderLyrics>, OmniError>> {
        Box::pin(async { Ok(None) })
    }
    fn login_code(&self) -> BoxFuture<'_, Result<LoginCode, OmniError>> {
        unsupported()
    }
    fn check_login<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<LoginState, OmniError>> {
        unsupported()
    }
    fn account(&self) -> BoxFuture<'_, Result<Option<Account>, OmniError>> {
        unsupported()
    }
    fn logout(&self) -> BoxFuture<'_, Result<(), OmniError>> {
        unsupported()
    }
    fn session(&self) -> Option<String> {
        None
    }
    fn restore_session(&self, _: &str) -> Result<(), OmniError> {
        Ok(())
    }
    fn user_playlists<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Vec<Collection>, OmniError>> {
        unsupported()
    }
    fn collection_detail<'a>(&'a self, _: &'a Collection) -> BoxFuture<'a, Result<CollectionDetail, OmniError>> {
        unsupported()
    }
    fn set_subscribed<'a>(&'a self, _: &'a Collection, _: bool) -> BoxFuture<'a, Result<(), OmniError>> {
        unsupported()
    }
    fn favorite_albums(&self) -> BoxFuture<'_, Result<Vec<Collection>, OmniError>> {
        unsupported()
    }
    fn radio_feed(&self, _: u32) -> BoxFuture<'_, Result<RadioFeed, OmniError>> {
        unsupported()
    }
    fn explore(&self) -> BoxFuture<'_, Result<Vec<ExploreSection>, OmniError>> {
        unsupported()
    }
    fn sized_cover(&self, _: &str, _: u32) -> Option<String> {
        None
    }
}

/// Order and switches come from the settings table; missing sources and unknown keys fall back to the defaults.
#[test]
fn the_settings_table_decides_the_order() {
    let mut settings = UnlockSettings::default();
    assert_eq!(
        settings.order(),
        [UnlockSource::Netease, UnlockSource::Bodian],
        "fork 的默认表：聚合 API 与波点开，酷我关"
    );
    settings.set_server(UnlockSource::Bodian, false);
    settings.set_server(UnlockSource::Kuwo, true);
    assert_eq!(settings.order(), [UnlockSource::Netease, UnlockSource::Kuwo]);
    let edited =
        UnlockSettings { enabled: true, servers: vec![UnlockServer { key: UnlockSource::Kuwo, enabled: true }] };
    let table = edited.servers();
    assert_eq!(table.len(), UnlockSource::ALL.len());
    assert!(table.iter().any(|server| server.key == UnlockSource::Bodian && server.enabled));
}
