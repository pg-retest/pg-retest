use std::borrow::Cow;
use std::sync::Arc;

use dashmap::DashMap;

/// Global shared ID map for cross-session correlation.
pub struct IdMap {
    inner: Arc<DashMap<String, String>>,
}

impl Clone for IdMap {
    fn clone(&self) -> Self {
        IdMap {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl IdMap {
    pub fn new() -> Self {
        IdMap {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn register(&self, captured: String, replayed: String) {
        self.inner.insert(captured, replayed);
    }

    pub fn get(&self, captured: &str) -> Option<String> {
        self.inner.get(captured).map(|v| v.value().clone())
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn substitute<'a>(&self, sql: &'a str) -> (Cow<'a, str>, usize) {
        if self.inner.is_empty() {
            return (Cow::Borrowed(sql), 0);
        }
        super::substitute::substitute_ids(sql, &self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        let map = IdMap::new();
        map.register("42".into(), "1001".into());
        assert_eq!(map.get("42"), Some("1001".into()));
        assert_eq!(map.get("99"), None);
    }

    #[test]
    fn test_map_len() {
        let map = IdMap::new();
        assert!(map.is_empty());
        map.register("42".into(), "1001".into());
        map.register("43".into(), "1002".into());
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn test_clone_shares_state() {
        let map1 = IdMap::new();
        let map2 = map1.clone();
        map1.register("42".into(), "1001".into());
        assert_eq!(map2.get("42"), Some("1001".into()));
    }

    #[tokio::test]
    async fn test_concurrent_register() {
        let map = IdMap::new();
        let mut handles = Vec::new();
        for task_id in 0..10u64 {
            let m = map.clone();
            handles.push(tokio::spawn(async move {
                for i in 0..100u64 {
                    m.register(
                        format!("{}_{}", task_id, i),
                        format!("new_{}_{}", task_id, i),
                    );
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(map.len(), 1000);
    }
}
