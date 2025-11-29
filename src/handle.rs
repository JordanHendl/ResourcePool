use core::fmt;
use std::alloc::{Layout, alloc_zeroed};
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

use bytemuck::{Pod, Zeroable};

/// Lightweight typed reference to an item stored in a [`Pool`].
///
/// A `Handle<T>` can be copied freely and is typically returned by
/// [`Pool::insert`].  It becomes invalid once the associated item is
/// released from the pool.
///
/// # Examples
/// ```no_run
/// # use resource_pool::{Handle, Pool};
/// let mut pool = Pool::new(4);
/// let handle = pool.insert(42u32).unwrap();
/// assert!(handle.valid());
/// assert_eq!(*pool.get_ref(handle).unwrap(), 42);
/// pool.release(handle);
/// // handle slot is now free for reuse
/// ```
#[repr(C)]
pub struct Handle<T: ?Sized> {
    /// Slot index within the pool.
    pub slot: u16,
    pub generation: u16,
    phantom: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .field("phantom", &self.phantom)
            .finish()
    }
}

impl<T> Handle<T> {
    /// Returns `true` if this handle refers to a valid pool entry.
    ///
    /// The default handle created by `Handle::default()` is considered
    /// invalid.
    pub fn valid(&self) -> bool {
        return self.slot != std::u16::MAX && self.generation != std::u16::MAX;
    }

    /// Creates a new handle from a slot and generation.
    ///
    /// # Safety
    /// This function does not validate that the given slot and
    /// generation actually correspond to a live item in a pool.  It is
    /// intended to be used by `Pool` internals; constructing handles with
    /// arbitrary values may yield dangling references.
    pub fn new(slot: u16, generation: u16) -> Self {
        Self {
            slot,
            generation,
            phantom: PhantomData,
        }
    }
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot && self.generation == other.generation
    }
}

impl<T> Eq for Handle<T> {}

impl<T> Hash for Handle<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.slot.hash(state);
        self.generation.hash(state);
    }
}

impl<T> Default for Handle<T> {
    fn default() -> Self {
        Self {
            slot: std::u16::MAX,
            generation: std::u16::MAX,
            phantom: PhantomData,
        }
    }
}

unsafe impl<T: 'static> Zeroable for Handle<T> {}
unsafe impl<T: 'static> Pod for Handle<T> {}

struct ItemList<T> {
    items: *mut T,
    end: *mut T,
    imported: bool,
    phantom: PhantomData<T>,
}

impl<T> ItemList<T> {
    fn new(len: u32) -> Self {
        unsafe {
            let byte_size = len as usize * std::mem::size_of::<T>();
            let layout = Layout::from_size_align(byte_size, 1).unwrap();
            let ptr = alloc_zeroed(layout);
            Self {
                items: ptr as *mut T,
                end: (ptr as *mut T).offset(len as isize),
                phantom: PhantomData::default(),
                imported: false,
            }
        }
    }

    fn new_from_prealloc(ptr: *mut u8, len: u32) -> Self {
        Self {
            items: ptr as *mut T,
            end: unsafe { (ptr as *mut T).offset(len as isize) },
            phantom: PhantomData::default(),
            imported: true,
        }
    }

    fn byte_size(&self) -> usize {
        return self.len() * std::mem::size_of::<T>();
    }

    //    fn as_slice(&self) -> &[T] {
    //        return unsafe { std::slice::from_raw_parts(self.items, self.len()) };
    //    }
    //
    //    fn as_slice_mut(&self) -> &mut [T] {
    //        return unsafe { std::slice::from_raw_parts_mut(self.items, self.len()) };
    //    }

    fn expand(&mut self, amt: usize) {
        if !self.imported {
            let len = self.len() + amt;
            unsafe {
                let byte_size = len as usize * std::mem::size_of::<T>();
                let layout = Layout::from_size_align(byte_size, 1).unwrap();
                let ptr = alloc_zeroed(layout);

                let src = std::slice::from_raw_parts(self.items as *const u8, self.byte_size());
                let dst = std::slice::from_raw_parts_mut(ptr, byte_size);

                dst[0..src.len()].copy_from_slice(src);

                self.items = ptr as *mut T;
                self.end = self.items.offset(len as isize);
            }
        }
    }

    fn len(&self) -> usize {
        return unsafe { self.end.offset_from(self.items) as usize };
    }

    //    fn free(&self) {
    //        if !self.imported {
    //            let byte_size = self.len() as usize * std::mem::size_of::<T>();
    //            let layout = Layout::from_size_align(byte_size, std::mem::size_of::<T>()).unwrap();
    //            unsafe { dealloc(self.items as *mut u8, layout) };
    //        }
    //    }

    fn iter(&self) -> ItemListRef<'_, T> {
        ItemListRef {
            holder: self,
            curr: 0,
        }
    }
    fn iter_mut(&mut self) -> ItemListRefMut<'_, T> {
        ItemListRefMut {
            holder: self,
            curr: 0,
        }
    }
}

impl<T> IndexMut<usize> for ItemList<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        let v = unsafe { self.items.offset(index as isize) };
        return unsafe { v.as_mut().unwrap() };
    }
}

impl<T> Index<usize> for ItemList<T> {
    type Output = T;
    fn index(&self, index: usize) -> &Self::Output {
        let v = unsafe { self.items.offset(index as isize) };
        return unsafe { v.as_mut().unwrap() };
    }
}

struct ItemListRef<'a, T> {
    holder: &'a ItemList<T>,
    curr: usize,
}

struct ItemListRefMut<'a, T> {
    holder: &'a mut ItemList<T>,
    curr: usize,
}

impl<'a, T> Iterator for ItemListRefMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.curr != self.holder.len() {
            let ptr = self.holder.items;
            let c = self.curr;
            self.curr += 1;
            return Some(unsafe { ptr.offset(c as isize).as_mut().unwrap() });
        } else {
            return None;
        }
    }

    fn count(self) -> usize
    where
        Self: Sized,
    {
        self.holder.len() - self.curr
    }
}

impl<'a, T> Iterator for ItemListRef<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.curr != self.holder.len() {
            let c = self.curr;
            self.curr += 1;
            return Some(&self.holder[c]);
        } else {
            return None;
        }
    }

    fn count(self) -> usize
    where
        Self: Sized,
    {
        self.holder.len() - self.curr
    }
}

impl<'a, T> IntoIterator for &'a ItemList<T> {
    type Item = &'a T;

    type IntoIter = ItemListRef<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        ItemListRef {
            holder: self,
            curr: 0,
        }
    }
}

/// Growable collection that hands out [`Handle`]s for stored items.
///
/// The pool maintains generation counters to validate handles and
/// recycles freed slots.  It is not thread-safe and expects exclusive
/// access when mutated.
///
/// # Examples
/// ```no_run
/// # use resource_pool::Pool;
/// let mut pool = Pool::new(16);
/// let handle = pool.insert(String::from("hello")).unwrap();
/// if let Some(text) = pool.get_ref(handle) {
///     println!("{}", text);
/// }
/// pool.release(handle);
/// ```
pub struct Pool<T> {
    items: ItemList<T>,
    empty: Vec<u32>,
    generation: Vec<u16>,
}

impl<T> Default for Pool<T> {
    fn default() -> Self {
        const INITIAL_SIZE: usize = 1024;
        let mut p = Pool {
            items: ItemList::new(INITIAL_SIZE as u32),
            empty: Vec::with_capacity(INITIAL_SIZE),
            generation: vec![0; INITIAL_SIZE],
        };

        p.empty = (0..(INITIAL_SIZE) as u32).collect();
        assert!(!p.generation.is_empty());
        return p;
    }
}
impl<T> Pool<T> {
    /// Creates a new pool with the given starting capacity.
    pub fn new(initial_size: usize) -> Self {
        let mut p = Pool {
            items: ItemList::new(initial_size as u32),
            empty: Vec::with_capacity(initial_size),
            generation: vec![0; initial_size],
        };

        assert!(!p.generation.is_empty());
        p.empty = (0..(initial_size) as u32).collect();
        return p;
    }

    /// Creates a pool that manages a pre-allocated memory block.
    ///
    /// # Safety
    /// The caller must ensure that `ptr` points to a valid, writable
    /// memory region capable of holding `len` items of type `G` and that
    /// it lives for the lifetime of the pool.
    pub fn new_preallocated<G>(ptr: *mut G, len: usize) -> Self {
        let mut p = Pool {
            items: ItemList::new_from_prealloc(ptr as *mut u8, len as u32),
            empty: Vec::with_capacity(len),
            generation: vec![0; len],
        };

        p.empty = (0..(len) as u32).collect();
        return p;
    }

    /// Returns a slice of indices representing free slots in the pool.
    pub fn get_empty(&self) -> &[u32] {
        &self.empty
    }

    /// Inserts an item into the pool, returning a [`Handle`] if
    /// successful.
    ///
    /// The pool will automatically expand if full.
    pub fn insert(&mut self, item: T) -> Option<Handle<T>> {
        const DEFAULT_EXPAND_AMT: usize = 1024;
        if let Some(empty_slot) = self.empty.pop() {
            self.items[empty_slot as usize] = item;

            assert!(!self.generation.is_empty());
            return Some(Handle {
                slot: empty_slot as u16,
                generation: self.generation[empty_slot as usize],
                phantom: PhantomData,
            });
        } else {
            self.expand(DEFAULT_EXPAND_AMT);
            if let Some(empty_slot) = self.empty.pop() {
                self.items[empty_slot as usize] = item;

                assert!(!self.generation.is_empty());
                return Some(Handle {
                    slot: empty_slot as u16,
                    generation: self.generation[empty_slot as usize],
                    phantom: PhantomData,
                });
            }
        }
        return None;
    }

    /// Inserts an item into the pool, returning a [`Handle`] if
    /// successful.
    ///
    /// The pool will automatically expand if full.
    pub fn insert_at(&mut self, item: T, slot: usize) -> Option<Handle<T>> {
        if let Some(idx) = self.empty.iter().position(|a| *a == slot as u32) {
            self.items[slot as usize] = item;
            self.empty.remove(idx);
            assert!(!self.generation.is_empty());
            return Some(Handle {
                slot: slot as u16,
                generation: self.generation[slot as usize],
                phantom: PhantomData,
            });
        }
        return None;
    }

    /// Grows the pool by `amount` additional slots.
    pub fn expand(&mut self, amount: usize) {
        let old_len = self.items.len();
        self.items.expand(amount);

        if self.items.len() > old_len {
            self.generation.resize_with(self.items.len(), || 0);
            for i in old_len..(self.items.len()) {
                self.empty.push(i as u32);
            }
        }
    }

    /// Returns the total number of slots currently managed by the pool.
    pub fn len(&self) -> usize {
        return self.items.len();
    }

    /// Calls `func` for each occupied handle in the pool.
    pub fn for_each_occupied_handle<F>(&self, func: F)
    where
        F: Fn(Handle<T>),
    {
        for (i, _) in self.items.iter().enumerate() {
            let c = i as u32;
            if !self.empty.contains(&c) {
                let h = Handle::<T> {
                    slot: i as u16,
                    generation: self.generation[i],
                    phantom: Default::default(),
                };
                func(h);
            }
        }
    }

    /// Mutable variant of [`Pool::for_each_occupied_handle`].
    pub fn for_each_occupied_handle_mut<F>(&self, mut func: F)
    where
        F: FnMut(Handle<T>),
    {
        for (i, _) in self.items.iter().enumerate() {
            let c = i as u32;
            if !self.empty.contains(&c) {
                let h = Handle::<T> {
                    slot: i as u16,
                    generation: self.generation[i],
                    phantom: Default::default(),
                };
                func(h);
            }
        }
    }

    /// Calls `func` for each occupied item reference.
    pub fn for_each_unoccupied<F>(&self, mut func: F)
    where
        F: FnMut(&T, usize),
    {
        for (iota, i) in self.empty.iter().enumerate() {
            func(&self.items[*i as usize], iota);
        }
    }

    /// Calls `func` for each occupied item reference.
    pub fn for_each_occupied<F>(&self, mut func: F)
    where
        F: FnMut(&T),
    {
        for (i, item) in self.items.iter().enumerate() {
            let c = i as u32;
            if !self.empty.contains(&c) {
                func(item);
            }
        }
    }

    /// Calls `func` for each occupied mutable reference.
    pub fn for_each_occupied_mut<F>(&mut self, mut func: F)
    where
        F: FnMut(&mut T),
    {
        for (i, item) in self.items.iter_mut().enumerate() {
            let c = i as u32;
            if !self.empty.contains(&c) {
                func(item);
            }
        }
    }

    /// Releases a handle, making its slot available for reuse.
    pub fn release(&mut self, item: Handle<T>) {
        self.empty.push(item.slot as u32);
    }

    /// Returns an immutable reference to the item associated with `item`.
    pub fn get_ref(&self, item: Handle<T>) -> Option<&T> {
        assert!(item.valid());
        assert!(self.items.len() != 0);
        assert!(!self.generation.is_empty());
        let slot = item.slot as u32;
        if self.generation[slot as usize] == item.generation {
            return Some(&self.items[slot as usize]);
        } else {
            None
        }
    }

    /// Returns a mutable reference to the item associated with `item`.
    pub fn get_mut_ref(&mut self, item: Handle<T>) -> Option<&mut T> {
        assert!(item.valid());
        assert!(!self.generation.is_empty());
        let slot = item.slot as usize;
        if self.generation[slot] == item.generation {
            return Some(&mut self.items[slot as usize]);
        } else {
            None
        }
    }

    /// Clears all entries and resets generation counters.
    pub fn clear(&mut self) {
        self.empty = (0..(self.items.len()) as u32).collect();
        self.generation.fill(0);
        assert!(!self.generation.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_pool() {
        const TEST_AMT: usize = 2048;
        #[derive(Default, Debug)]
        struct S {
            _big_data: [u32; 16],
        }
        let mut pool: Pool<S> = Pool::new(TEST_AMT);
        assert!(pool.items.len() == TEST_AMT);

        let mut p = Vec::new();

        for _it in 0..TEST_AMT + 1 {
            p.push(pool.insert(S::default()).expect("ASSERT: Should insert."));
        }

        assert!(pool.items.len() > TEST_AMT);

        pool.for_each_occupied_mut(|f| {
            f._big_data[0] = 5;
        });
    }

    #[test]
    #[serial]
    fn test_clear_allows_inserts() {
        #[derive(Default)]
        struct S {
            _val: u32,
        }
        let mut pool: Pool<S> = Pool::new(1);
        assert!(pool.insert(S::default()).is_some());
        pool.clear();
        assert!(pool.insert(S::default()).is_some());
    }

    #[test]
    #[serial]
    fn test_pool_imported() {
        const TEST_AMT: usize = 2048;
        #[derive(Default)]
        struct S {
            _big_data: [u32; 16],
        }
        let byte_size = TEST_AMT as usize * std::mem::size_of::<S>();
        let layout = Layout::from_size_align(byte_size, 1).unwrap();
        let ptr = unsafe { alloc_zeroed(layout) };

        let mut pool: Pool<S> = Pool::new_preallocated(ptr, TEST_AMT);
        assert!(pool.items.len() == TEST_AMT);

        let mut p = Vec::new();

        for _it in 0..TEST_AMT {
            p.push(pool.insert(S::default()).expect("ASSERT: Should insert."));
        }

        for _it in 0..TEST_AMT {
            assert!(pool.insert(S::default()) == None);
        }
        assert!(pool.items.len() == TEST_AMT);
    }
}
