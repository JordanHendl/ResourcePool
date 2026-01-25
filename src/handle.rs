use core::fmt;
use std::alloc::{Layout, alloc_zeroed};
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};
use std::sync::Mutex;

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

struct DynamicItemList {
    items: *mut u8,
    end: *mut u8,
    item_size: usize,
    item_align: usize,
    imported: bool,
}

impl DynamicItemList {
    fn new(len: u32, item_size: usize, item_align: usize) -> Self {
        unsafe {
            let byte_size = len as usize * item_size;
            let layout = Layout::from_size_align(byte_size, item_align).unwrap();
            let ptr = alloc_zeroed(layout);
            Self {
                item_size,
                item_align,
                items: ptr as *mut u8,
                end: (ptr as *mut u8).offset(len as isize * item_size as isize),
                imported: false,
            }
        }
    }

    fn new_from_prealloc(ptr: *mut u8, len: u32, item_size: usize, item_align: usize) -> Self {
        Self {
            items: ptr as *mut u8,
            end: unsafe { (ptr as *mut u8).offset(len as isize * item_size as isize) },
            imported: true,
            item_size,
            item_align,
        }
    }

    fn at<T>(&self, idx: usize) -> &T {
        debug_assert!(std::mem::size_of::<T>() == self.item_size);
        let ptr = unsafe { (self.items as *const T).offset(idx as isize) };
        unsafe { &*ptr }
    }

    fn at_mut<T>(&mut self, idx: usize) -> &mut T {
        debug_assert!(std::mem::size_of::<T>() == self.item_size);
        let ptr = unsafe { (self.items as *mut T).offset(idx as isize) };
        unsafe { &mut *ptr }
    }

    unsafe fn at_mut_unchecked<T>(&self, idx: usize) -> &mut T {
        debug_assert!(std::mem::size_of::<T>() == self.item_size);
        let ptr = (self.items as *mut T).offset(idx as isize);
        &mut *ptr
    }

    fn as_slice<T>(&self) -> &[T] {
        debug_assert!(std::mem::size_of::<T>() == self.item_size);
        return unsafe { std::slice::from_raw_parts(self.items as *const T, self.len()) };
    }

    fn as_slice_mut<T>(&self) -> &mut [T] {
        debug_assert!(std::mem::size_of::<T>() == self.item_size);
        return unsafe { std::slice::from_raw_parts_mut(self.items as *mut T, self.len()) };
    }

    fn byte_size(&self) -> usize {
        return self.len() * self.item_size;
    }

    fn expand(&mut self, amt: usize) {
        if !self.imported {
            let len = self.len() + amt;
            unsafe {
                let byte_size = len as usize * self.item_size;
                let layout = Layout::from_size_align(byte_size, self.item_align).unwrap();
                let ptr = alloc_zeroed(layout);

                let src = std::slice::from_raw_parts(self.items as *const u8, self.byte_size());
                let dst = std::slice::from_raw_parts_mut(ptr, byte_size);

                dst[0..src.len()].copy_from_slice(src);

                self.items = ptr as *mut u8;
                self.end = self.items.offset(len as isize * self.item_size as isize);
            }
        }
    }

    fn len(&self) -> usize {
        return unsafe { self.end.offset_from(self.items) as usize } / self.item_size;
    }

    //    fn free(&self) {
    //        if !self.imported {
    //            let byte_size = self.len() as usize * std::mem::size_of::<T>();
    //            let layout = Layout::from_size_align(byte_size, std::mem::size_of::<T>()).unwrap();
    //            unsafe { dealloc(self.items as *mut u8, layout) };
    //        }
    //    }
}

impl IndexMut<usize> for DynamicItemList {
    fn index_mut(&mut self, index: usize) -> &mut u8 {
        let v = unsafe { self.items.offset(index as isize) };
        return unsafe { v.as_mut().unwrap() };
    }
}

impl Index<usize> for DynamicItemList {
    type Output = u8;
    fn index(&self, index: usize) -> &Self::Output {
        let v = unsafe { self.items.offset(index as isize) };
        return unsafe { v.as_mut().unwrap() };
    }
}

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
            let layout = Layout::from_size_align(byte_size, std::mem::align_of::<T>()).unwrap();
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

    unsafe fn at_mut_unchecked(&self, idx: usize) -> &mut T {
        let ptr = self.items.offset(idx as isize);
        &mut *ptr
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

pub struct DynamicPool {
    items: DynamicItemList,
    empty: Vec<u32>,
    generation: Vec<u16>,
    locks: Vec<Mutex<()>>,
}

impl Default for DynamicPool {
    fn default() -> Self {
        const INITIAL_SIZE: usize = 1024;
        let mut p = DynamicPool {
            items: DynamicItemList::new(INITIAL_SIZE as u32, 1, 1),
            empty: Vec::with_capacity(INITIAL_SIZE),
            generation: vec![0; INITIAL_SIZE],
            locks: (0..INITIAL_SIZE).map(|_| Mutex::new(())).collect(),
        };

        p.empty = (0..(INITIAL_SIZE) as u32).collect();
        assert!(!p.generation.is_empty());
        return p;
    }
}
impl DynamicPool {
    /// Creates a new pool with the given starting capacity.
    pub fn new(initial_size: usize, item_size: u32, align_size: u32) -> Self {
        let mut p = DynamicPool {
            items: DynamicItemList::new(
                initial_size as u32,
                item_size as usize,
                align_size as usize,
            ),
            empty: Vec::with_capacity(initial_size),
            generation: vec![0; initial_size],
            locks: (0..initial_size).map(|_| Mutex::new(())).collect(),
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
    pub fn new_preallocated<G>(ptr: *mut G, len: usize, item_size: u32, item_align: u32) -> Self {
        let mut p = DynamicPool {
            items: DynamicItemList::new_from_prealloc(
                ptr as *mut u8,
                len as u32,
                item_size as usize,
                item_align as usize,
            ),
            empty: Vec::with_capacity(len),
            generation: vec![0; len],
            locks: (0..len).map(|_| Mutex::new(())).collect(),
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
    pub fn insert<T>(&mut self, item: T) -> Option<Handle<T>> {
        const DEFAULT_EXPAND_AMT: usize = 1024;
        if let Some(empty_slot) = self.empty.pop() {
            *self.items.at_mut::<T>(empty_slot as usize) = item;

            assert!(!self.generation.is_empty());
            return Some(Handle {
                slot: empty_slot as u16,
                generation: self.generation[empty_slot as usize],
                phantom: PhantomData,
            });
        } else {
            self.expand(DEFAULT_EXPAND_AMT);
            if let Some(empty_slot) = self.empty.pop() {
                *self.items.at_mut::<T>(empty_slot as usize) = item;

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
    pub fn insert_at<T>(&mut self, item: T, slot: usize) -> Option<Handle<T>> {
        if let Some(idx) = self.empty.iter().position(|a| *a == slot as u32) {
            *self.items.at_mut::<T>(slot as usize) = item;
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
            self.locks
                .resize_with(self.items.len(), || Mutex::new(()));
            for i in old_len..(self.items.len()) {
                self.empty.push(i as u32);
            }
        }
    }

    /// Returns the total number of slots currently managed by the pool.
    pub fn len(&self) -> usize {
        return self.items.len();
    }

    //    /// Calls `func` for each occupied handle in the pool.
    //    pub fn for_each_occupied_handle<F>(&self, func: F)
    //    where
    //        F: Fn(Handle<T>),
    //    {
    //        for (i, _) in self.items.iter().enumerate() {
    //            let c = i as u32;
    //            if !self.empty.contains(&c) {
    //                let h = Handle::<T> {
    //                    slot: i as u16,
    //                    generation: self.generation[i],
    //                    phantom: Default::default(),
    //                };
    //                func(h);
    //            }
    //        }
    //    }
    //
    //    /// Mutable variant of [`Pool::for_each_occupied_handle`].
    //    pub fn for_each_occupied_handle_mut<F>(&self, mut func: F)
    //    where
    //        F: FnMut(Handle<T>),
    //    {
    //        for (i, _) in self.items.iter().enumerate() {
    //            let c = i as u32;
    //            if !self.empty.contains(&c) {
    //                let h = Handle::<T> {
    //                    slot: i as u16,
    //                    generation: self.generation[i],
    //                    phantom: Default::default(),
    //                };
    //                func(h);
    //            }
    //        }
    //    }
    //
    //    /// Calls `func` for each occupied item reference.
    //    pub fn for_each_unoccupied<F>(&self, mut func: F)
    //    where
    //        F: FnMut(&T, usize),
    //    {
    //        for (iota, i) in self.empty.iter().enumerate() {
    //            func(&self.items[*i as usize], iota);
    //        }
    //    }
    //
    //    /// Calls `func` for each occupied item reference.
    //    pub fn for_each_occupied<F>(&self, mut func: F)
    //    where
    //        F: FnMut(&T),
    //    {
    //        for (i, item) in self.items.iter().enumerate() {
    //            let c = i as u32;
    //            if !self.empty.contains(&c) {
    //                func(item);
    //            }
    //        }
    //    }
    //
    //    /// Calls `func` for each occupied mutable reference.
    //    pub fn for_each_occupied_mut<F>(&mut self, mut func: F)
    //    where
    //        F: FnMut(&mut T),
    //    {
    //        for (i, item) in self.items.iter_mut().enumerate() {
    //            let c = i as u32;
    //            if !self.empty.contains(&c) {
    //                func(item);
    //            }
    //        }
    //    }

    /// Releases a handle, making its slot available for reuse.
    pub fn release<T>(&mut self, item: Handle<T>) {
        debug_assert!(std::mem::align_of::<T>() == self.items.item_align);
        debug_assert!(std::mem::size_of::<T>() == self.items.item_size);
        self.empty.push(item.slot as u32);
        self.generation[item.slot as usize] += 1;
    }

    /// Returns an immutable reference to the item associated with `item`.
    pub fn get_ref<T>(&self, item: Handle<T>) -> Option<&T> {
        debug_assert!(std::mem::size_of::<T>() == self.items.item_size);
        debug_assert!(std::mem::align_of::<T>() == self.items.item_align);
        assert!(item.valid());
        assert!(self.items.len() != 0);
        assert!(!self.generation.is_empty());
        let slot = item.slot as u32;
        if self.generation[slot as usize] == item.generation {
            return Some(self.items.at::<T>(slot as usize));
        } else {
            None
        }
    }

    /// Returns a mutable reference to the item associated with `item`.
    #[deprecated(note = "use with_mut to avoid &mut escaping synchronization")]
    pub fn get_mut_ref<T>(&mut self, item: Handle<T>) -> Option<&mut T> {
        debug_assert!(std::mem::size_of::<T>() == self.items.item_size);
        debug_assert!(std::mem::align_of::<T>() == self.items.item_align);
        assert!(item.valid());
        assert!(!self.generation.is_empty());
        let slot = item.slot as usize;
        if self.generation[slot] == item.generation {
            return Some(self.items.at_mut::<T>(slot as usize));
        } else {
            None
        }
    }

    /// Calls `f` with a mutable reference to the item associated with `item`.
    pub fn with_mut<T, R>(&self, item: Handle<T>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        debug_assert!(std::mem::size_of::<T>() == self.items.item_size);
        debug_assert!(std::mem::align_of::<T>() == self.items.item_align);
        assert!(item.valid());
        assert!(!self.generation.is_empty());
        let slot = item.slot as usize;
        if self.generation.get(slot)? != &item.generation {
            return None;
        }
        let _guard = self.locks.get(slot)?.lock().ok()?;
        if self.generation[slot] == item.generation {
            // Safety: the per-slot lock guarantees exclusive access to this slot.
            let item_ref = unsafe { self.items.at_mut_unchecked::<T>(slot) };
            return Some(f(item_ref));
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
    occupied: Vec<bool>,
    locks: Vec<Mutex<()>>,
}

impl<T> Default for Pool<T> {
    fn default() -> Self {
        const INITIAL_SIZE: usize = 1024;
        let mut p = Pool {
            items: ItemList::new(INITIAL_SIZE as u32),
            empty: Vec::with_capacity(INITIAL_SIZE),
            generation: vec![0; INITIAL_SIZE],
            occupied: vec![false; INITIAL_SIZE],
            locks: (0..INITIAL_SIZE).map(|_| Mutex::new(())).collect(),
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
            occupied: vec![false; initial_size],
            locks: (0..initial_size).map(|_| Mutex::new(())).collect(),
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
            occupied: vec![false; len],
            locks: (0..len).map(|_| Mutex::new(())).collect(),
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
            self.occupied[empty_slot as usize] = true;
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
                self.occupied[empty_slot as usize] = true;

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
            self.occupied[idx as usize] = true;
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
            self.occupied.resize_with(self.items.len(), || false);
            self.generation.resize_with(self.items.len(), || 0);
            self.locks
                .resize_with(self.items.len(), || Mutex::new(()));
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
        for (i, &is_occupied) in self.occupied.iter().enumerate() {
            if is_occupied {
                func(Handle {
                    slot: i as u16,
                    generation: self.generation[i],
                    phantom: PhantomData,
                });
            }
        }
    }

    /// Mutable variant of [`Pool::for_each_occupied_handle`].
    pub fn for_each_occupied_handle_mut<F>(&self, mut func: F)
    where
        F: FnMut(Handle<T>),
    {
        for (i, &is_occupied) in self.occupied.iter().enumerate() {
            if is_occupied {
                func(Handle {
                    slot: i as u16,
                    generation: self.generation[i],
                    phantom: PhantomData,
                });
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
        for (item, &is_occupied) in self.items.iter().zip(self.occupied.iter()) {
            if is_occupied {
                func(item);
            }
        }
    }

    /// Calls `func` for each occupied mutable reference.
    pub fn for_each_occupied_mut<F>(&mut self, mut func: F)
    where
        F: FnMut(&mut T),
    {
        for (item, &is_occupied) in self.items.iter_mut().zip(self.occupied.iter()) {
            if is_occupied {
                func(item);
            }
        }
    }

    /// Releases a handle, making its slot available for reuse.
    pub fn release(&mut self, item: Handle<T>) {
        self.empty.push(item.slot as u32);
        self.generation[item.slot as usize] += 1;
        self.occupied[item.slot as usize] = false;
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
    #[deprecated(note = "use with_mut to avoid &mut escaping synchronization")]
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

    /// Calls `f` with a mutable reference to the item associated with `item`.
    pub fn with_mut<R>(&self, item: Handle<T>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        assert!(item.valid());
        assert!(!self.generation.is_empty());
        let slot = item.slot as usize;
        if self.generation.get(slot)? != &item.generation {
            return None;
        }
        let _guard = self.locks.get(slot)?.lock().ok()?;
        if self.generation[slot] == item.generation {
            // Safety: the per-slot lock guarantees exclusive access to this slot.
            let item_ref = unsafe { self.items.at_mut_unchecked(slot) };
            return Some(f(item_ref));
        } else {
            None
        }
    }

    /// Clears all entries and resets generation counters.
    pub fn clear(&mut self) {
        self.empty = (0..(self.items.len()) as u32).collect();
        self.generation.fill(0);
        self.occupied.fill(false);
        assert!(!self.generation.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // ---------------- DynamicPool tests ----------------

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct DynS(u32);

    #[test]
    fn dynamic_pool_insert_and_get_roundtrip() {
        use std::mem::{align_of, size_of};

        const INITIAL: usize = 8;
        const TOTAL: usize = INITIAL + 10;

        let mut pool =
            DynamicPool::new(INITIAL, size_of::<DynS>() as u32, align_of::<DynS>() as u32);
        assert_eq!(pool.len(), INITIAL);

        let mut handles = Vec::new();
        for i in 0..TOTAL {
            let h = pool.insert(DynS(i as u32)).expect("insert should succeed");
            handles.push(h);
        }

        // Pool should have expanded
        assert!(pool.len() >= TOTAL);

        // All handles should give back the right values
        for (i, h) in handles.iter().enumerate() {
            let v = pool.get_ref(*h).expect("handle should be valid");
            assert_eq!(v.0, i as u32);
        }
    }

    #[test]
    fn dynamic_pool_expand_adds_slots_to_empty_list() {
        use std::mem::{align_of, size_of};

        const INITIAL: usize = 4;
        const EXTRA: usize = 16;

        let mut pool =
            DynamicPool::new(INITIAL, size_of::<DynS>() as u32, align_of::<DynS>() as u32);

        let old_len = pool.len();
        assert_eq!(old_len, INITIAL);

        let old_empty_len = pool.get_empty().len();

        pool.expand(EXTRA);

        let new_len = pool.len();
        assert!(new_len >= old_len + EXTRA);

        let new_empty_len = pool.get_empty().len();
        assert!(new_empty_len > old_empty_len);
    }

    #[test]
    fn dynamic_pool_preallocated_cannot_auto_expand() {
        use std::alloc::{Layout, alloc_zeroed};
        use std::mem::{align_of, size_of};

        const N: usize = 8;

        let byte_size = N * size_of::<DynS>();
        let layout = Layout::from_size_align(byte_size, align_of::<DynS>()).unwrap();
        let ptr = unsafe { alloc_zeroed(layout) as *mut DynS };

        let mut pool = DynamicPool::new_preallocated(
            ptr,
            N,
            size_of::<DynS>() as u32,
            align_of::<DynS>() as u32,
        );
        assert_eq!(pool.len(), N);

        // Fill the pool fully
        for i in 0..N {
            assert!(pool.insert(DynS(i as u32)).is_some());
        }

        // Further inserts must fail (no expansion for preallocated)
        for _ in 0..N {
            assert!(pool.insert(DynS(999)).is_none());
        }

        assert_eq!(pool.len(), N);
    }

    /// This encodes the *intended* generation semantics:
    /// reusing a slot must bump generation so old handles become invalid.
    #[test]
    fn dynamic_pool_reuse_slot_bumps_generation() {
        use std::mem::{align_of, size_of};

        let mut pool = DynamicPool::new(1, size_of::<DynS>() as u32, align_of::<DynS>() as u32);

        let h1 = pool.insert(DynS(1)).expect("first insert should succeed");
        let slot = h1.slot;

        pool.release(h1);

        let h2 = pool.insert(DynS(2)).expect("second insert should succeed");

        // same slot reused
        assert_eq!(h2.slot, slot);

        // generation should differ (this will currently FAIL with your code)
        assert_ne!(
            h1.generation, h2.generation,
            "generation must change when a slot is reused"
        );

        // Old handle must now be considered invalid
        assert!(
            pool.get_ref(h1).is_none(),
            "old handle should no longer be valid after reuse"
        );

        // New handle should see the new value
        let v = pool.get_ref(h2).expect("new handle should be valid");
        assert_eq!(v.0, 2);
    }

    /// In debug builds, using a mismatched T with `get_ref` should trip
    /// the debug_assert on size/alignment.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic]
    fn dynamic_pool_type_mismatch_panics_in_debug() {
        use std::mem::{align_of, size_of};

        let mut pool = DynamicPool::new(1, size_of::<u32>() as u32, align_of::<u32>() as u32);
        let h_u32 = pool.insert(123u32).expect("insert should succeed");

        // SAFETY: We're intentionally misusing types here to check debug
        // assertions. This should panic due to the size/align debug_asserts
        // in get_ref::<T>.
        let _ = pool.get_ref::<u64>(Handle::new(h_u32.slot, h_u32.generation));
    }

    // ---------------- DynamicItemList tests ----------------

    #[test]
    fn dynamic_item_list_basic_len_and_byte_size() {
        use std::mem::{align_of, size_of};

        const N: u32 = 16;
        let list = DynamicItemList::new(N, size_of::<u32>(), align_of::<u32>());

        // len() should be in *items*, not bytes.
        assert_eq!(list.len(), N as usize);
        assert_eq!(list.byte_size(), N as usize * size_of::<u32>());

        // sanity-check that end - items matches byte_size
        let bytes = unsafe { list.end.offset_from(list.items) as usize };
        assert_eq!(bytes, list.byte_size());
    }

    #[test]
    fn dynamic_item_list_at_roundtrip() {
        use std::mem::{align_of, size_of};

        const N: u32 = 8;
        let mut list = DynamicItemList::new(N, size_of::<u64>(), align_of::<u64>());

        // Write some values
        for i in 0..N as usize {
            *list.at_mut::<u64>(i) = (i as u64) * 10;
        }

        // Read back
        for i in 0..N as usize {
            assert_eq!(*list.at::<u64>(i), (i as u64) * 10);
        }
    }

    #[test]
    fn dynamic_item_list_expand_preserves_contents_and_grows_len() {
        use std::mem::{align_of, size_of};

        const INITIAL: u32 = 8;
        const EXTRA: usize = 16;

        let mut list = DynamicItemList::new(INITIAL, size_of::<u32>(), align_of::<u32>());

        for i in 0..INITIAL as usize {
            *list.at_mut::<u32>(i) = (100 + i) as u32;
        }

        let old_len = list.len();
        assert_eq!(old_len, INITIAL as usize);

        list.expand(EXTRA);

        // Length should have grown by EXACTLY EXTRA items.
        assert_eq!(list.len(), old_len + EXTRA);

        // Old values preserved.
        for i in 0..INITIAL as usize {
            assert_eq!(*list.at::<u32>(i), (100 + i) as u32);
        }
    }

    #[test]
    fn dynamic_item_list_alignment_respected_on_initial_alloc() {
        use std::mem::{align_of, size_of};

        const N: u32 = 4;
        let list = DynamicItemList::new(N, size_of::<u64>(), align_of::<u64>());

        let ptr_val = list.items as usize;
        assert_eq!(
            ptr_val % align_of::<u64>(),
            0,
            "items pointer should be aligned for u64"
        );
    }

    #[test]
    fn dynamic_item_list_alignment_preserved_on_expand() {
        use std::mem::{align_of, size_of};

        const N: u32 = 4;
        let mut list = DynamicItemList::new(N, size_of::<u64>(), align_of::<u64>());

        let initial_ptr_val = list.items as usize;
        assert_eq!(initial_ptr_val % align_of::<u64>(), 0);

        list.expand(16);

        let new_ptr_val = list.items as usize;
        assert_eq!(
            new_ptr_val % align_of::<u64>(),
            0,
            "expand() must allocate with the same alignment"
        );
    }

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
