//! Persistent app data in `~/.local/share/kargoslop/state.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    /// Kargo projects shown in the sidebar.
    #[serde(default)]
    pub projects: Vec<String>,
    /// Argo CD cluster name → kube context, for clusters whose context name doesn't end in
    /// the cluster name.
    #[serde(default)]
    pub contexts: BTreeMap<String, String>,
    /// The user chose "Don't ask again" for adding kargoslop.app to ~/Applications.
    #[serde(default)]
    pub skip_app_prompt: bool,
}

pub fn data_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("kargoslop")
}

impl Store {
    fn path() -> PathBuf {
        data_dir().join("state.json")
    }

    pub fn load() -> Store {
        std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap()).context("create data dir")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}
