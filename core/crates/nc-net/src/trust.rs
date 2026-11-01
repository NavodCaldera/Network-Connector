//! Trusted peers, keyed by peer key ([`PeerId`]) only. Names and addresses are never trust anchors.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use libp2p::PeerId;

use crate::NetError;

/// Set of paired peers. Cheap to clone; clones share state.
///
/// With a path, every change is written to a JSON file (list of peer id strings), so pairing survives
/// restarts ("reconnect" in task 0.6). Phase 0: plain file; Phase 1 moves this into `nc-store`.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    inner: Arc<RwLock<BTreeSet<PeerId>>>,
    path: Option<PathBuf>,
}

impl TrustStore {
    /// Empty in-memory store.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Loads the store from `path` (missing file = empty) and persists changes there.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, NetError> {
        let path = path.into();
        let mut set = BTreeSet::new();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let ids: Vec<String> = serde_json::from_slice(&bytes)
                    .map_err(|e| NetError::Protocol(format!("trust file: {e}")))?;
                for id in ids {
                    set.insert(
                        id.parse()
                            .map_err(|_| NetError::Protocol("trust file: bad peer id".into()))?,
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Self {
            inner: Arc::new(RwLock::new(set)),
            path: Some(path),
        })
    }

    /// True if `peer` has been paired.
    pub fn contains(&self, peer: &PeerId) -> bool {
        self.inner.read().map(|s| s.contains(peer)).unwrap_or(false)
    }

    /// Marks `peer` as trusted. Only the pairing code path should call this.
    pub fn add(&self, peer: PeerId) -> Result<(), NetError> {
        self.mutate(|s| {
            s.insert(peer);
        })
    }

    /// Removes `peer` (unpair).
    pub fn remove(&self, peer: &PeerId) -> Result<(), NetError> {
        self.mutate(|s| {
            s.remove(peer);
        })
    }

    /// All trusted peers.
    pub fn peers(&self) -> Vec<PeerId> {
        self.inner
            .read()
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    fn mutate(&self, f: impl FnOnce(&mut BTreeSet<PeerId>)) -> Result<(), NetError> {
        let snapshot = {
            let mut g = self
                .inner
                .write()
                .map_err(|_| NetError::Protocol("trust store poisoned".into()))?;
            f(&mut g);
            g.iter().map(|p| p.to_string()).collect::<Vec<_>>()
        };
        if let Some(path) = &self.path {
            let json = serde_json::to_vec_pretty(&snapshot)
                .map_err(|e| NetError::Protocol(e.to_string()))?;
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("nc-net-trust-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trust.json");
        let _ = std::fs::remove_file(&path);
        let p = PeerId::random();
        let s = TrustStore::open(&path).unwrap();
        assert!(!s.contains(&p));
        s.add(p).unwrap();
        let s2 = TrustStore::open(&path).unwrap();
        assert!(s2.contains(&p));
        s2.remove(&p).unwrap();
        assert!(!TrustStore::open(&path).unwrap().contains(&p));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
