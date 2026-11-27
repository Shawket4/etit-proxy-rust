//! English road names, by OSM way id, for the fee lookup's road lists.
//!
//! Valhalla serves a road under its default-language name, Arabic in Egypt,
//! and has no request option for `name:en`. `valhalla/build.sh` therefore
//! exports `apex-road-names.json` from the same extract the tiles came from:
//! every motorway–tertiary way (and link) that carries `name:en`. The road
//! list looks each edge's way id up here.
//!
//! The file ships with each Valhalla release, so its path
//! (`VALHALLA_ROAD_NAMES`, normally `/opt/valhalla/current/…`) can change
//! content under a running service. [`EnglishNames`] re-reads it when the
//! file's modification time changes. A missing or unreadable file means no
//! English names, never an error: the lists fall back to the Arabic name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use serde::Deserialize;

#[derive(Debug, Default)]
pub struct RoadNames {
    by_way: HashMap<i64, String>,
}

impl RoadNames {
    pub fn english(&self, way_id: i64) -> Option<&str> {
        self.by_way.get(&way_id).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_way.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_way.is_empty()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    fn parse(raw: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct File {
            names: HashMap<String, String>,
        }
        let f: File = serde_json::from_slice(raw)?;
        let by_way = f
            .names
            .into_iter()
            .filter_map(|(k, v)| {
                let v = v.trim().to_string();
                Some((k.parse::<i64>().ok()?, v)).filter(|(_, v)| !v.is_empty())
            })
            .collect();
        Ok(Self { by_way })
    }
}

/// The names file, re-read whenever it changes on disk.
#[derive(Clone, Default)]
pub struct EnglishNames {
    path: Option<PathBuf>,
    names: Arc<ArcSwap<RoadNames>>,
    seen: Arc<Mutex<Option<SystemTime>>>,
}

impl EnglishNames {
    pub fn new(path: Option<PathBuf>) -> Self {
        let n = Self { path, ..Self::default() };
        n.current();
        n
    }

    /// The names as of now: re-read first if the file changed (or appeared).
    pub fn current(&self) -> Arc<RoadNames> {
        if let Some(path) = &self.path {
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            let mut seen = self.seen.lock();
            if modified.is_some() && modified != *seen {
                match RoadNames::load(path) {
                    Ok(n) => {
                        tracing::info!(names = n.len(), path = %path.display(), "English road names loaded");
                        self.names.store(Arc::new(n));
                        *seen = modified;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, path = %path.display(), "English road names not loaded");
                    }
                }
            }
        }
        self.names.load_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_by_way_id_and_skip_junk() {
        let n = RoadNames::parse(br#"{"tag":"name:en","ways":3,"names":{"276379253":"Al Ein Al Sokhna Road","x":"bad id","5":"  "}}"#).unwrap();
        assert_eq!(n.english(276379253), Some("Al Ein Al Sokhna Road"));
        assert_eq!(n.len(), 1, "a bad id and a blank name are dropped");
        assert!(RoadNames::parse(b"{}").is_err());
    }

    #[test]
    fn a_new_file_under_the_same_path_is_picked_up_and_a_missing_one_is_no_names() {
        let dir = std::env::temp_dir().join(format!("road-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("apex-road-names.json");

        let none = EnglishNames::new(Some(path.clone()));
        assert!(none.current().is_empty(), "no file yet: no names, no error");

        std::fs::write(&path, r#"{"names":{"1":"One Road"}}"#).unwrap();
        assert_eq!(none.current().english(1), Some("One Road"), "appeared after start");

        // A deploy swaps the release: same path, new content, new mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, r#"{"names":{"1":"One Road","2":"Two Road"}}"#).unwrap();
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        assert_eq!(none.current().english(2), Some("Two Road"));

        // Broken file: the last good names stay.
        std::fs::write(&path, "not json").unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later + std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(none.current().english(2), Some("Two Road"));

        assert!(EnglishNames::new(None).current().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
