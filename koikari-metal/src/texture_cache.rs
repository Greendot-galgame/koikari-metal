//! CPU-only texture residency and frame-aware LRU policy.
//!
//! All cache hits must consult this map, never a second "uploaded once" map:
//! eviction invalidates both the GPU object and its dimensions atomically.
use std::collections::{HashMap, HashSet};

struct Entry<T> {
    value: T,
    bytes: usize,
}

pub(crate) struct TextureCache<T> {
    entries: HashMap<String, Entry<T>>,
    /// Oldest to newest. Every resident key appears exactly once.
    recency: Vec<String>,
    /// Resources uploaded or queued for drawing in the current frame.
    pinned: HashSet<String>,
    bytes: usize,
    budget: usize,
}

impl<T> TextureCache<T> {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: Vec::new(),
            pinned: HashSet::new(),
            bytes: 0,
            budget,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&T> {
        self.entries.get(key).map(|entry| &entry.value)
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub(crate) fn begin_frame(&mut self) {
        self.pinned.clear();
        // Do not trim here: we do not yet know which previous-frame resources
        // the new frame needs. Trim on insertion, after those resources can be
        // pinned by draw(), to avoid needlessly reloading a visible background.
    }

    /// Insertions are immediately pinned so a large texture cannot evict itself
    /// before the caller has had a chance to queue its draw.
    pub(crate) fn insert(&mut self, key: String, value: T, bytes: usize) {
        if let Some(previous) = self.entries.insert(key.clone(), Entry { value, bytes }) {
            self.bytes -= previous.bytes;
        }
        self.bytes += bytes;
        self.pin(&key);
        self.evict();
    }

    pub(crate) fn pin(&mut self, key: &str) -> bool {
        if !self.entries.contains_key(key) {
            return false;
        }
        self.pinned.insert(key.to_owned());
        if let Some(index) = self.recency.iter().position(|candidate| candidate == key) {
            self.recency.remove(index);
        }
        self.recency.push(key.to_owned());
        true
    }

    fn evict(&mut self) {
        let mut index = 0;
        while self.bytes > self.budget && index < self.recency.len() {
            if self.pinned.contains(&self.recency[index]) {
                index += 1;
                continue;
            }
            let key = self.recency.remove(index);
            if let Some(removed) = self.entries.remove(&key) {
                self.bytes -= removed.bytes;
            }
        }
        // A frame's working set can exceed the budget. Keeping that frame
        // intact is preferable to rendering a character's eyes without a face.
        // The next insertion may reclaim these entries after begin_frame().
    }
}

#[cfg(test)]
mod tests {
    use super::TextureCache;

    #[test]
    fn eviction_removes_dimensions_and_value_together_and_allows_reupload() {
        let mut cache = TextureCache::new(8);
        cache.insert("face".into(), (1, 2), 8);
        cache.begin_frame();
        cache.insert("background".into(), (2, 1), 8);
        assert_eq!(cache.get("face"), None);
        assert!(!cache.contains_key("face"));
        cache.begin_frame();
        cache.insert("face".into(), (1, 2), 8);
        assert_eq!(cache.get("face"), Some(&(1, 2)));
        assert_eq!(cache.get("background"), None);
        assert_eq!(cache.bytes, 8);
    }

    #[test]
    fn queued_background_body_and_face_survive_later_uploads() {
        let mut cache = TextureCache::new(12);
        cache.insert("old_text".into(), (), 4);
        cache.begin_frame();
        cache.insert("background".into(), (), 8);
        cache.insert("body".into(), (), 8);
        cache.insert("face".into(), (), 8);
        cache.insert("text".into(), (), 4);
        assert!(!cache.contains_key("old_text"));
        for key in ["background", "body", "face", "text"] {
            assert!(cache.contains_key(key), "lost current-frame {key}");
        }
        assert_eq!(cache.bytes, 28);
    }

    #[test]
    fn over_budget_upload_cannot_evict_itself() {
        let mut cache = TextureCache::new(4);
        cache.insert("large".into(), (), 32);
        assert!(cache.contains_key("large"));
        cache.begin_frame();
        cache.insert("small".into(), (), 4);
        assert!(!cache.contains_key("large"));
        assert!(cache.contains_key("small"));
        assert_eq!(cache.bytes, 4);
    }

    #[test]
    fn reused_current_frame_texture_is_pinned_and_updates_lru() {
        let mut cache = TextureCache::new(8);
        cache.insert("background".into(), (), 4);
        cache.insert("old".into(), (), 4);
        cache.begin_frame();
        assert!(cache.pin("background"));
        cache.insert("face".into(), (), 4);
        assert!(cache.contains_key("background"));
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("face"));
        assert_eq!(cache.bytes, 8);
    }

    #[test]
    fn replacement_keeps_accounting_and_recency_unique() {
        let mut cache = TextureCache::new(20);
        cache.insert("text".into(), 1, 4);
        cache.insert("text".into(), 2, 8);
        assert_eq!(cache.bytes, 8);
        assert_eq!(cache.get("text"), Some(&2));
        assert_eq!(cache.recency, vec!["text"]);
        cache.begin_frame();
        cache.insert("another".into(), 3, 16);
        assert!(!cache.contains_key("text"));
        assert_eq!(cache.bytes, 16);
    }

    #[test]
    fn missing_lookup_or_pin_does_not_create_a_resident_entry() {
        let mut cache: TextureCache<()> = TextureCache::new(4);
        assert!(!cache.pin("missing"));
        assert!(!cache.contains_key("missing"));
        assert!(cache.recency.is_empty());
        assert!(cache.pinned.is_empty());
    }
}
