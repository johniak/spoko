//! In-memory object server for tests.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use spoko_crypto::ObjectId;

use crate::Result;
use crate::transfer::ObjectServer;

#[derive(Default)]
pub struct MemServer {
    pub objects: Mutex<HashMap<ObjectId, Vec<u8>>>,
    pub missing_calls: AtomicUsize,
    pub put_objects: AtomicUsize,
    /// When set, `get` fails after this many objects were returned (simulated crash).
    pub fail_get_after: Mutex<Option<usize>>,
    returned: AtomicUsize,
}

impl MemServer {
    pub fn puts(&self) -> usize {
        self.put_objects.load(Ordering::SeqCst)
    }
    pub fn reset_counters(&self) {
        self.put_objects.store(0, Ordering::SeqCst);
        self.missing_calls.store(0, Ordering::SeqCst);
    }
}

impl ObjectServer for MemServer {
    fn missing(&self, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
        self.missing_calls.fetch_add(1, Ordering::SeqCst);
        let objs = self.objects.lock().expect("mem");
        Ok(ids
            .iter()
            .filter(|i| !objs.contains_key(i))
            .copied()
            .collect())
    }
    fn put(&self, batch: &[(ObjectId, Vec<u8>)]) -> Result<()> {
        let mut objs = self.objects.lock().expect("mem");
        for (i, b) in batch {
            objs.insert(*i, b.clone());
        }
        self.put_objects.fetch_add(batch.len(), Ordering::SeqCst);
        Ok(())
    }
    fn get(&self, ids: &[ObjectId]) -> Result<Vec<(ObjectId, Vec<u8>)>> {
        let objs = self.objects.lock().expect("mem");
        let out: Vec<_> = ids.iter().filter_map(|i| objs.get(i).map(|b| (*i, b.clone()))).collect();
        // counted under the lock so parallel workers cannot all slip under the limit
        let before = self.returned.fetch_add(out.len(), Ordering::SeqCst);
        if let Some(limit) = *self.fail_get_after.lock().expect("mem")
            && before + out.len() > limit
        {
            return Err(crate::ClientError::Server("simulated outage".into()));
        }
        Ok(out)
    }
}
