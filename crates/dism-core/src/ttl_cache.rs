use std::sync::RwLock;
use std::time::{Duration, Instant};

pub(crate) struct TtlCache<T> {
    ttl: Duration,
    state: RwLock<CacheState<T>>,
}

struct CacheState<T> {
    generation: u64,
    value: Option<(Instant, T)>,
}

impl<T: Clone> TtlCache<T> {
    pub(crate) const fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            state: RwLock::new(CacheState {
                generation: 0,
                value: None,
            }),
        }
    }

    pub(crate) fn get_or_load(&self, load: impl FnOnce() -> T) -> T {
        let generation = {
            let state = self.state.read().unwrap_or_else(|error| error.into_inner());
            if let Some(value) = self.fresh_value(&state) {
                return value.clone();
            }
            state.generation
        };
        let computed = load();
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(value) = self.fresh_value(&state) {
            return value.clone();
        }
        if state.generation == generation {
            state.value = Some((Instant::now(), computed.clone()));
        }
        computed
    }

    pub(crate) fn invalidate(&self) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.value = None;
    }

    fn fresh_value<'a>(&self, state: &'a CacheState<T>) -> Option<&'a T> {
        state
            .value
            .as_ref()
            .filter(|(loaded, _)| loaded.elapsed() < self.ttl)
            .map(|(_, value)| value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_fresh_values_and_reloads_expired_values() {
        let cached = TtlCache::new(Duration::from_secs(10));
        assert_eq!(cached.get_or_load(|| 1), 1);
        assert_eq!(
            cached.get_or_load(|| panic!("fresh cache should be reused")),
            1
        );
        let expired = TtlCache::new(Duration::ZERO);
        assert_eq!(expired.get_or_load(|| 1), 1);
        assert_eq!(expired.get_or_load(|| 2), 2);
    }

    #[test]
    fn invalidation_prevents_an_inflight_load_from_refreshing_the_cache() {
        let cache = TtlCache::new(Duration::from_secs(10));
        assert_eq!(
            cache.get_or_load(|| {
                cache.invalidate();
                1
            }),
            1
        );
        assert_eq!(cache.get_or_load(|| 2), 2);
        cache.invalidate();
        assert_eq!(cache.get_or_load(|| 3), 3);
    }
}
