//! Config files (TOML). Written atomically; on Unix with mode 0600 because
//! they hold the private key.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use annex_proto::control::{ModePref, SourcePref};
use annex_proto::keys::{StaticKeypair, hex32};
use annex_proto::pairing::PeerIdentity;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config I/O: {0}")]
    Io(#[from] io::Error),
    #[error("config parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config write: {0}")]
    Write(#[from] toml::ser::Error),
}

/// The paired peer, as stored.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PairedPeer {
    pub id: String,
    pub name: String,
    #[serde(with = "hex32")]
    pub public_key: [u8; 32],
}

impl From<PeerIdentity> for PairedPeer {
    fn from(p: PeerIdentity) -> Self {
        PairedPeer { id: p.id, name: p.name, public_key: p.public_key }
    }
}

/// u64 as a decimal string: TOML integers are signed 64-bit.
pub mod u64_str {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MonitorPrefs {
    #[serde(with = "u64_str")]
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub priority: u8,
    #[serde(default)]
    pub pinned_mode: ModePref,
    #[serde(default)]
    pub source: SourcePref,
}

/// Windows receiver config: `%APPDATA%\Annex\config.toml`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReceiverConfig {
    pub receiver_id: String,
    pub keys: StaticKeypair,
    #[serde(default)]
    pub paired: Option<PairedPeer>,
    #[serde(default)]
    pub show_stats: bool,
    #[serde(default = "default_control_port")]
    pub control_port: u16,
    #[serde(default = "default_video_port")]
    pub video_port: u16,
    #[serde(default)]
    pub monitors: Vec<MonitorPrefs>,
}

fn default_control_port() -> u16 {
    annex_proto::DEFAULT_CONTROL_PORT
}

fn default_video_port() -> u16 {
    annex_proto::DEFAULT_VIDEO_PORT
}

fn default_true() -> bool {
    true
}

impl ReceiverConfig {
    pub fn fresh() -> Self {
        ReceiverConfig {
            receiver_id: uuid::Uuid::new_v4().to_string(),
            keys: StaticKeypair::generate(),
            paired: None,
            show_stats: false,
            control_port: default_control_port(),
            video_port: default_video_port(),
            monitors: Vec::new(),
        }
    }
}

/// Mac sender config: `~/Library/Application Support/Annex/config.toml`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SenderConfig {
    pub sender_id: String,
    pub keys: StaticKeypair,
    #[serde(default)]
    pub paired: Option<PairedPeer>,
    /// Receiver monitor id → virtual display slot. Slots fix the display
    /// identity, so macOS remembers each monitor's arrangement.
    #[serde(default)]
    pub slots: BTreeMap<String, u32>,
    /// Draw the cursor on the receiver from position updates instead of
    /// baking it into the video.
    #[serde(default = "default_true")]
    pub client_cursor: bool,
    /// Override of the encoder budget (encoded megapixels per second).
    #[serde(default)]
    pub encoder_budget_mpx: Option<f64>,
}

impl SenderConfig {
    pub fn fresh() -> Self {
        SenderConfig {
            sender_id: uuid::Uuid::new_v4().to_string(),
            keys: StaticKeypair::generate(),
            paired: None,
            slots: BTreeMap::new(),
            client_cursor: true,
            encoder_budget_mpx: None,
        }
    }

    /// Slot for a receiver monitor, assigning the lowest free one on first sight.
    pub fn slot_for(&mut self, monitor_id: u64) -> u32 {
        let key = monitor_id.to_string();
        if let Some(s) = self.slots.get(&key) {
            return *s;
        }
        let mut s = 0;
        while self.slots.values().any(|v| *v == s) {
            s += 1;
        }
        self.slots.insert(key, s);
        s
    }
}

pub fn load<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(toml::from_str(&s)?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Loads `path`, or creates it from `fresh()` if missing.
pub fn load_or_create<T: Serialize + DeserializeOwned>(
    path: &Path,
    fresh: impl FnOnce() -> T,
) -> Result<T, ConfigError> {
    if let Some(c) = load(path)? {
        return Ok(c);
    }
    let c = fresh();
    save(path, &c)?;
    Ok(c)
}

pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), ConfigError> {
    let text = toml::to_string_pretty(value)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp: PathBuf = path.with_extension("toml.tmp");
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("annex-cfg-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut c = ReceiverConfig::fresh();
        c.monitors.push(MonitorPrefs {
            id: u64::MAX - 3,
            name: "DELL".into(),
            enabled: true,
            priority: 0,
            pinned_mode: ModePref::Sharp,
            source: SourcePref::Mirror { display_uuid: "X".into() },
        });
        c.paired = Some(PairedPeer { id: "m".into(), name: "Mac".into(), public_key: [1; 32] });
        save(&path, &c).unwrap();
        let back: ReceiverConfig = load(&path).unwrap().unwrap();
        assert_eq!(back.monitors, c.monitors);
        assert_eq!(back.keys, c.keys);
        assert_eq!(back.paired, c.paired);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn slots_are_stable() {
        let mut c = SenderConfig::fresh();
        assert_eq!(c.slot_for(10), 0);
        assert_eq!(c.slot_for(20), 1);
        assert_eq!(c.slot_for(10), 0);
    }
}
