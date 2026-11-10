//! A small FNV-1a hasher for string-keyed interpreter tables (environment
//! bindings, object properties, global bindings). The standard `RandomState`
//! hasher is collision-resistant and costs more than a lookup on a short
//! identifier, and these tables are keyed by script identifiers the engine
//! does not need DoS-hardening for. Dependency-free and deterministic.

use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::hash::Hasher;

pub(crate) struct FnvHasher(u64);

impl Default for FnvHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }
}

pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FnvHasher>>;
