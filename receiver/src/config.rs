use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Stable id so the phone can find this PC again after its IP changes.
    pub id: String,
    #[serde(default)]
    pub devices: Vec<PairedDevice>,
    #[serde(default)]
    pub options: crate::actions::ActionOpts,
    /// Set once the app has asked to open the firewall, so it asks only once.
    #[serde(default)]
    pub firewall_asked: bool,
    /// Random friendly name ("Flaky USB") shown on phones, picked on first run.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    pub name: String,
    /// Only a hash of the token is stored on disk.
    pub token_sha256: String,
    pub paired_at: u64,
    /// The phone's own stable id (newer phones send it), so reconnecting replaces this entry.
    #[serde(default)]
    pub phone_id: Option<String>,
}

pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut b);
    hex::encode(b)
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("scansend")
        .join("receiver.json")
}

impl Config {
    pub fn load_or_create() -> Result<Self> {
        let p = path();
        if let Ok(text) = std::fs::read_to_string(&p) {
            return serde_json::from_str(&text).with_context(|| format!("reading {}", p.display()));
        }
        let cfg = Config { id: random_hex(8), devices: vec![], options: Default::default(), firewall_asked: false, name: None };
        cfg.save()?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }

    pub fn device_for_token(&self, token: &str) -> Option<&PairedDevice> {
        let h = hash_token(token);
        self.devices.iter().find(|d| d.token_sha256 == h)
    }
}
