use super::Pool;

use crate::*;

pub struct ResourceList<T> {
    pub pool: Pool<T>,
    pub entries: Vec<Handle<T>>,
}

impl<T> Default for ResourceList<T> {
    fn default() -> Self {
        Self {
            pool: Default::default(),
            entries: Default::default(),
        }
    }
}

#[allow(dead_code)]
impl<T> ResourceList<T> {
    pub fn new(size: usize) -> Self {
        Self {
            pool: Pool::new(size),
            entries: Vec::with_capacity(size),
        }
    }

    pub fn push(&mut self, v: T) -> Handle<T> {
        let h = self.pool.insert(v).unwrap();
        self.entries.push(h);
        h
    }

    pub fn release(&mut self, h: Handle<T>) {
        if let Some(idx) = self.entries.iter().position(|a| a.slot == h.slot) {
            self.entries.remove(idx);
            self.pool.release(h);
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get_ref(&self, h: Handle<T>) -> &T {
        self.pool.get_ref(h).unwrap()
    }

    pub fn get_ref_mut(&mut self, h: Handle<T>) -> &mut T {
        self.pool.get_mut_ref(h).unwrap()
    }

    #[allow(dead_code)]
    pub fn for_each_occupied<F>(&self, func: F)
    where
        F: Fn(&T),
    {
        for item in &self.entries {
            let r = self.pool.get_ref(item.clone()).unwrap();
            func(r);
        }
    }

    pub fn for_each_handle<F>(&self, mut func: F)
    where
        F: FnMut(Handle<T>),
    {
        for h in &self.entries {
            func(*h);
        }
    }

    #[allow(dead_code)]
    pub fn for_each_occupied_mut<F>(&mut self, mut func: F)
    where
        F: FnMut(&T),
    {
        for item in &self.entries {
            let r = self.pool.get_mut_ref(item.clone()).unwrap();
            func(r);
        }
    }
}

//impl ResourceList<ShaderResource> {
//    pub fn to_indexed_resources(&self) -> Vec<IndexedResource> {
//        let mut v = Vec::new();
//        let mut slot = 0;
//        self.pool.for_each_occupied(|f| {
//            slot += 1;
//            v.push(IndexedResource {
//                resource: f.clone(),
//                slot: slot - 1,
//            });
//        });
//
//        v
//    }
//}
