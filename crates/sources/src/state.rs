//! A plugin's own key-value state: cursors, `since` marks, ETags (§18.1).
//!
//! [`PluginState`] is the contract. [`MemoryState`] is the in-memory
//! implementation for tests and for plugins until the store-backed one
//! (SRC-1, the `source_state` table) is wired in. Both enforce the same
//! limits, so a plugin that passes against one fits in the other. The
//! name avoids `lodestar_store::SourceState`, which is a source's row in
//! the `sources` table.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::plugin::BoxFuture;

/// Bytes in a key.
pub const MAX_KEY: usize = 128;
/// Bytes in a value.
pub const MAX_VALUE: usize = 64 * 1024;
/// Keys per plugin.
pub const MAX_KEYS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// An empty key or one longer than [`MAX_KEY`].
    BadKey,
    /// A value longer than [`MAX_VALUE`].
    ValueTooLarge,
    /// A new key would pass [`MAX_KEYS`].
    Full,
    /// The backing store failed.
    Store(String),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::BadKey => write!(f, "state key empty or over {MAX_KEY} bytes"),
            StateError::ValueTooLarge => write!(f, "state value over {MAX_VALUE} bytes"),
            StateError::Full => write!(f, "plugin state holds {MAX_KEYS} keys already"),
            StateError::Store(e) => write!(f, "plugin state store: {e}"),
        }
    }
}

impl std::error::Error for StateError {}

/// One plugin's state. Each call is atomic on its own; use
/// [`compare_and_set`](PluginState::compare_and_set) when two tasks of one
/// plugin may race on a key.
pub trait PluginState: Send + Sync + std::fmt::Debug {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, StateError>>;

    fn set<'a>(&'a self, key: &'a str, value: Vec<u8>) -> BoxFuture<'a, Result<(), StateError>>;

    /// Removing an absent key succeeds.
    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StateError>>;

    /// Writes `new` (or removes the key for `None`) only if the current
    /// value equals `expected` (`None`: the key is absent). Returns whether
    /// it wrote.
    fn compare_and_set<'a>(
        &'a self,
        key: &'a str,
        expected: Option<&'a [u8]>,
        new: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<bool, StateError>>;
}

/// Checks a key and value against the limits.
pub fn check(key: &str, value: Option<&[u8]>) -> Result<(), StateError> {
    if key.is_empty() || key.len() > MAX_KEY {
        return Err(StateError::BadKey);
    }
    if value.is_some_and(|v| v.len() > MAX_VALUE) {
        return Err(StateError::ValueTooLarge);
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct MemoryState {
    map: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryState {
    pub fn new() -> MemoryState {
        MemoryState::default()
    }

    /// The number of keys held.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>> {
        // A panic while holding the lock can't leave the map half-written:
        // every write is a single insert or remove.
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write(
        map: &mut HashMap<String, Vec<u8>>,
        key: &str,
        value: Option<Vec<u8>>,
    ) -> Result<(), StateError> {
        match value {
            None => {
                map.remove(key);
            }
            Some(v) => {
                if !map.contains_key(key) && map.len() >= MAX_KEYS {
                    return Err(StateError::Full);
                }
                map.insert(key.to_owned(), v);
            }
        }
        Ok(())
    }
}

impl PluginState for MemoryState {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, StateError>> {
        Box::pin(async move {
            check(key, None)?;
            Ok(self.lock().get(key).cloned())
        })
    }

    fn set<'a>(&'a self, key: &'a str, value: Vec<u8>) -> BoxFuture<'a, Result<(), StateError>> {
        Box::pin(async move {
            check(key, Some(&value))?;
            MemoryState::write(&mut self.lock(), key, Some(value))
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StateError>> {
        Box::pin(async move {
            check(key, None)?;
            MemoryState::write(&mut self.lock(), key, None)
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        key: &'a str,
        expected: Option<&'a [u8]>,
        new: Option<Vec<u8>>,
    ) -> BoxFuture<'a, Result<bool, StateError>> {
        Box::pin(async move {
            check(key, new.as_deref())?;
            let mut map = self.lock();
            if map.get(key).map(Vec::as_slice) != expected {
                return Ok(false);
            }
            MemoryState::write(&mut map, key, new)?;
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_set_remove() {
        let s = MemoryState::new();
        assert_eq!(s.get("cursor").await, Ok(None));
        s.set("cursor", b"42".to_vec()).await.unwrap();
        assert_eq!(s.get("cursor").await, Ok(Some(b"42".to_vec())));
        s.remove("cursor").await.unwrap();
        s.remove("cursor").await.unwrap();
        assert!(s.is_empty());
    }

    #[tokio::test]
    async fn compare_and_set_only_on_match() {
        let s = MemoryState::new();
        assert_eq!(
            s.compare_and_set("k", None, Some(b"a".to_vec())).await,
            Ok(true)
        );
        assert_eq!(
            s.compare_and_set("k", None, Some(b"b".to_vec())).await,
            Ok(false)
        );
        assert_eq!(
            s.compare_and_set("k", Some(b"a"), Some(b"b".to_vec()))
                .await,
            Ok(true)
        );
        assert_eq!(s.compare_and_set("k", Some(b"b"), None).await, Ok(true));
        assert_eq!(s.get("k").await, Ok(None));
    }

    #[tokio::test]
    async fn limits_hold() {
        let s = MemoryState::new();
        assert_eq!(s.get("").await, Err(StateError::BadKey));
        assert_eq!(
            s.set(&"k".repeat(MAX_KEY + 1), vec![]).await,
            Err(StateError::BadKey)
        );
        assert_eq!(
            s.set("k", vec![0; MAX_VALUE + 1]).await,
            Err(StateError::ValueTooLarge)
        );
        for i in 0..MAX_KEYS {
            s.set(&i.to_string(), vec![]).await.unwrap();
        }
        assert_eq!(s.set("one more", vec![]).await, Err(StateError::Full));
        // Overwriting an existing key is still allowed when full.
        s.set("0", b"x".to_vec()).await.unwrap();
        assert_eq!(s.len(), MAX_KEYS);
    }
}
