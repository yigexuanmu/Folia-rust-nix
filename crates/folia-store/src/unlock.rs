// Song unlock: alternate sources for songs a platform will not play in full, and their order.
//
// From the fork's `useSongUnlockSettingsStore` (storage keys `use_song_unlock` and `song_unlock_servers`): the
// whole feature has one switch, and every alternate source has its own, in the order the fork lists them.

use serde::{Deserialize, Deserializer, Serialize};

use crate::Domain;

/// One alternate source the unlock may ask, spelled as the fork stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnlockSource {
    Netease,
    Bodian,
    Kuwo,
}

impl UnlockSource {
    /// The table the settings and the resolver share, in the order the fork shows them.
    pub const ALL: [UnlockSource; 3] = [UnlockSource::Netease, UnlockSource::Bodian, UnlockSource::Kuwo];

    /// The stored spelling (`key` in the fork's settings).
    pub fn id(self) -> &'static str {
        match self {
            UnlockSource::Netease => "netease",
            UnlockSource::Bodian => "bodian",
            UnlockSource::Kuwo => "kuwo",
        }
    }

    /// Reads a stored spelling; `None` for a key this build does not know.
    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.id() == id)
    }

    pub fn label_key(self) -> &'static str {
        match self {
            UnlockSource::Netease => "options.songUnlockServerNetease",
            UnlockSource::Bodian => "options.songUnlockServerBodian",
            UnlockSource::Kuwo => "options.songUnlockServerKuwo",
        }
    }

    /// What the source is and at what bitrate it hands the song out.
    pub fn hint_key(self) -> &'static str {
        match self {
            UnlockSource::Netease => "options.songUnlockServerDescNetease",
            UnlockSource::Bodian => "options.songUnlockServerDescBodian",
            UnlockSource::Kuwo => "options.songUnlockServerDescKuwo",
        }
    }
}

/// One row of the source list: which source, and whether it may be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlockServer {
    pub key: UnlockSource,
    pub enabled: bool,
}

/// The fork's defaults: the aggregate API and the 320 kbps sub-platform on, the 128 kbps mobile one off.
const DEFAULT_SERVERS: [UnlockServer; 3] = [
    UnlockServer { key: UnlockSource::Netease, enabled: true },
    UnlockServer { key: UnlockSource::Bodian, enabled: true },
    UnlockServer { key: UnlockSource::Kuwo, enabled: false },
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnlockSettings {
    /// The fork's `useSongUnlock`: off, and no alternate source is asked at all.
    pub enabled: bool,
    /// What the file holds; read the table through [`UnlockSettings::servers`], which fills in new sources.
    #[serde(deserialize_with = "stored_servers")]
    pub servers: Vec<UnlockServer>,
}

/// Reads the stored table, dropping rows whose source this build does not know, so a file written by
/// a newer fork (or hand-edited) still loads every source it does know.
fn stored_servers<'de, D>(deserializer: D) -> Result<Vec<UnlockServer>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct StoredServer {
        key: String,
        #[serde(default)]
        enabled: bool,
    }

    let stored = Vec::<StoredServer>::deserialize(deserializer)?;
    Ok(stored
        .into_iter()
        .filter_map(|server| {
            UnlockSource::from_id(&server.key).map(|key| UnlockServer { key, enabled: server.enabled })
        })
        .collect())
}

impl Default for UnlockSettings {
    fn default() -> Self {
        Self { enabled: true, servers: DEFAULT_SERVERS.to_vec() }
    }
}

impl Domain for UnlockSettings {
    const FILE: &'static str = "unlock.toml";
    const SCHEMA_VERSION: u32 = 1;
}

impl UnlockSettings {
    /// The source table as the settings mean it: the default order, the stored switches where a key matches, a
    /// source added since the file was written taking its default. Unknown keys are dropped.
    pub fn servers(&self) -> Vec<UnlockServer> {
        DEFAULT_SERVERS
            .iter()
            .map(|default| {
                let stored = self.servers.iter().find(|server| server.key == default.key);
                UnlockServer { key: default.key, enabled: stored.map_or(default.enabled, |server| server.enabled) }
            })
            .collect()
    }

    /// The sources to ask, in order: the enabled ones, and none at all while the feature is off.
    pub fn order(&self) -> Vec<UnlockSource> {
        if !self.enabled {
            return Vec::new();
        }
        self.servers().into_iter().filter(|server| server.enabled).map(|server| server.key).collect()
    }

    pub fn server_enabled(&self, key: UnlockSource) -> bool {
        self.servers().iter().any(|server| server.key == key && server.enabled)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Flips one source, storing the whole table in its fixed order so the file never drifts from the defaults.
    pub fn set_server(&mut self, key: UnlockSource, enabled: bool) {
        self.servers = self
            .servers()
            .into_iter()
            .map(|server| if server.key == key { UnlockServer { key, enabled } } else { server })
            .collect();
    }
}
