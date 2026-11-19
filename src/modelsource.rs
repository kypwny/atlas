//! Where streamed model files come from. Same idea as [`crate::source`]: the
//! viewer only sees [`ModelProvider`]s, so other sources (for example an
//! encrypted archive, supplied by a separate add-on) plug in without changes.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct ModelListing {
    /// File name, which is also the model's name.
    pub name: String,
    pub key: String,
    pub size: u64,
}

pub trait ModelProvider: Send + Sync {
    fn label(&self) -> &str;
    fn list(&self) -> Result<Vec<ModelListing>>;
    fn read(&self, key: &str) -> Result<Vec<u8>>;
}

/// A folder of loose streamed model files.
pub struct LooseModels {
    label: String,
    dir: PathBuf,
}

impl LooseModels {
    pub fn new(label: impl Into<String>, dir: PathBuf) -> Self {
        Self { label: label.into(), dir }
    }
}

impl ModelProvider for LooseModels {
    fn label(&self) -> &str {
        &self.label
    }

    fn list(&self) -> Result<Vec<ModelListing>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir).with_context(|| format!("reading {}", self.dir.display()))? {
            let e = e?;
            let meta = e.metadata()?;
            if meta.is_file() {
                let name = e.file_name().to_string_lossy().into_owned();
                out.push(ModelListing { key: name.clone(), name, size: meta.len() });
            }
        }
        Ok(out)
    }

    fn read(&self, key: &str) -> Result<Vec<u8>> {
        std::fs::read(self.dir.join(key)).with_context(|| format!("reading {key}"))
    }
}

/// Finds `models` folders under a game folder (or inside one): the folder
/// itself, and `main`, `hires`, `raw` below it.
pub fn discover(root: &Path) -> Vec<Arc<dyn ModelProvider>> {
    let mut found: Vec<Arc<dyn ModelProvider>> = Vec::new();
    let mut seen = HashSet::new();
    let mut dirs = vec![root.to_path_buf()];
    for sub in ["main", "hires", "raw"] {
        dirs.push(root.join(sub));
    }
    for dir in dirs {
        let tag = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let models = if dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("models")) { dir.clone() } else { dir.join("models") };
        if models.is_dir() && seen.insert(models.canonicalize().unwrap_or_else(|_| models.clone())) {
            found.push(Arc::new(LooseModels::new(format!("{tag}/models"), models)));
        }
    }
    if let Some(store) = crate::ifs::IfsStore::shared(root) {
        found.extend(store.model_providers());
    }
    found
}

pub struct ModelEntry {
    pub name: String,
    pub provider: usize,
    pub key: String,
    pub size: u64,
}

pub struct ModelCatalog {
    pub providers: Vec<Arc<dyn ModelProvider>>,
    /// Sorted by name; a name present in several providers appears once per provider.
    pub entries: Vec<ModelEntry>,
}

pub fn scan(providers: Vec<Arc<dyn ModelProvider>>) -> ModelCatalog {
    let mut entries = Vec::new();
    for (pi, p) in providers.iter().enumerate() {
        match p.list() {
            Ok(list) => entries.extend(list.into_iter().map(|l| ModelEntry { name: l.name, provider: pi, key: l.key, size: l.size })),
            Err(e) => eprintln!("{}: {e:#}", p.label()),
        }
    }
    entries.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()).then(a.provider.cmp(&b.provider)));
    ModelCatalog { providers, entries }
}
