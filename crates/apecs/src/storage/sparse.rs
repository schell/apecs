//! Storage using a sparse set.
//!
//! ## Pros
//! * better than [`VecStorage`] on space
//! * 0(1) insert and remove
//!
//! ## Cons
//! * non-monotonic inserts cause the entities
use std::{
    iter::Map,
    slice::{Iter, IterMut},
};

use crate::storage::*;

use super::Entry;

pub struct SparseStorage<T> {
    sparse: Vec<usize>,
    dense: Vec<Entry<T>>,
    unsorted: bool,
    last_inserted_id: Option<usize>,
}

impl<T> Default for SparseStorage<T> {
    fn default() -> Self {
        Self {
            sparse: Default::default(),
            dense: Default::default(),
            unsorted: false,
            last_inserted_id: None,
        }
    }
}

impl<T> CanReadStorage for SparseStorage<T> {
    type Component = T;

    type Iter<'a> = Map<Iter<'a, Entry<T>>, fn(&Entry<T>) -> Entry<&T>> where T: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        let dense_id = self.sparse.get(id)?;
        Some(&self.dense.get(*dense_id)?.value)
    }

    fn iter(&self) -> Self::Iter<'_> {
        if self.unsorted {
            panic!(
                "{} is possibly not monotonic",
                std::any::type_name::<Self>()
            );
        }
        self.dense.iter().map(Entry::as_ref)
    }
}

impl<T> CanWriteStorage for SparseStorage<T> {
    type IterMut<'a> = Map<IterMut<'a, Entry<T>>, fn(&mut Entry<T>) -> Entry<&mut T>> where T: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        let dense_id = self.dense_id(id)?;
        Some(&mut self.dense.get_mut(dense_id)?.value)
    }

    fn insert(&mut self, id: usize, mut component: Self::Component) -> Option<Self::Component> {
        if id >= self.capacity() {
            // this is a brand new insert at the end of both arrays
            self.sparse.resize(id + 1, usize::MAX);
            self.sparse[id] = self.dense.len();
            self.dense.push(Entry {
                key: id,
                value: component,
            });
            self.last_inserted_id = Some(id);
            return None;
        }
        if let Some(dense_id) = self.dense_id(id) {
            // we already have this index stored, so no changes to order,
            std::mem::swap(&mut self.dense[dense_id].value, &mut component);
            self.last_inserted_id = Some(id);
            return Some(component);
        }
        // otherwise the sparse entry at id points to garbage
        // and the storage is now (possibly) unsorted
        self.sparse[id] = self.dense.len();
        self.dense.push(Entry {
            key: id,
            value: component,
        });
        if let Some(prev_id) = self.last_inserted_id {
            if prev_id > id {
                self.unsorted = true;
            }
        }
        self.last_inserted_id = Some(id);
        None
    }

    /// O(1) Removes the entry at the given key and returns it if
    /// possible, but leaves the storage in an unsorted state.
    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        if self.contains(id) {
            let dense_id = self.sparse[id];
            let removed = self.dense.swap_remove(dense_id).value;
            self.unsorted = true;
            if self.dense.len() > 0 && dense_id < self.dense.len() {
                let swapped_entry = &self.dense[dense_id];
                self.sparse[swapped_entry.key()] = dense_id;
            }
            self.sparse[id] = usize::MAX;
            Some(removed)
        } else {
            None
        }
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        if self.unsorted {
            self.sort();
        }
        self.dense.iter_mut().map(Entry::as_mut)
    }
}

impl<T> SparseStorage<T> {
    pub fn new_with_capacity(cap: usize) -> Self {
        let dense = vec![];
        let sparse = vec![usize::MAX; cap];

        Self {
            dense,
            sparse,
            unsorted: false,
            last_inserted_id: None,
        }
    }

    /// Return the current storage capacity
    pub(crate) fn capacity(&self) -> usize {
        self.sparse.len()
    }

    pub(crate) fn dense_id(&self, id: usize) -> Option<usize> {
        let key = self.sparse.get(id).map(|i| *i)?;
        if self.dense.get(key)?.key() == key {
            Some(key)
        } else {
            None
        }
    }

    pub(crate) fn contains(&self, id: usize) -> bool {
        id < self.sparse.len()
            && self.sparse[id] < self.dense.len()
            && self.dense[self.sparse[id]].key() == id
    }

    /// Returns an iterator over the dense keys in the order they
    /// were inserted or sorted.
    pub fn keys<'a>(&'a self) -> impl Iterator<Item = usize> + 'a {
        self.dense.iter().map(|e| e.key())
    }

    /// Returns an iterator over the sparse pointers.
    pub fn pointers<'a>(&'a self) -> impl Iterator<Item = usize> + 'a {
        self.sparse.iter().map(|i| *i)
    }

    /// Sort the dense vec, updating the sparse pointers.
    pub(crate) fn sort(&mut self) {
        self.dense.sort_by(|a, b| a.key().cmp(&b.key()));
        for (entry, i) in self.dense.iter().zip(0usize..) {
            self.sparse[entry.key()] = i;
        }
        self.unsorted = false;
    }
}

#[cfg(test)]
mod test {
    use crate::storage::*;

    #[test]
    pub fn sparse_sorting_problem() {
        let mut sparse: SparseStorage<&str> = SparseStorage::default();
        sparse.insert(0, "zero");
        sparse.insert(1, "one");
        sparse.insert(2, "two");
        sparse.insert(10, "ten");
        assert_eq!(sparse.get(0).unwrap(), &"zero");
        assert_eq!(sparse.get(1).unwrap(), &"one");
        assert_eq!(sparse.get(2).unwrap(), &"two");
        assert_eq!(sparse.get(10).unwrap(), &"ten");

        let keys = sparse.keys().collect::<Vec<_>>();
        assert_eq!(&keys, &[0, 1, 2, 10]);
        assert!(sparse.unsorted == false);
        let pointers = sparse.pointers().take(4).collect::<Vec<_>>();
        assert_eq!(&pointers, &[0, 1, 2, usize::MAX]);

        sparse.insert(6, "six");

        let keys = sparse.keys().collect::<Vec<_>>();
        assert_eq!(&keys, &[0, 1, 2, 10, 6]);
        assert!(sparse.unsorted);

        sparse.sort();
        let keys = sparse.keys().collect::<Vec<_>>();
        assert_eq!(&keys, &[0, 1, 2, 6, 10]);
        assert!(sparse.unsorted == false);
        assert_eq!(sparse.get(0).unwrap(), &"zero");
        assert_eq!(sparse.get(1).unwrap(), &"one");
        assert_eq!(sparse.get(2).unwrap(), &"two");
        assert_eq!(sparse.get(6).unwrap(), &"six");
        assert_eq!(sparse.get(10).unwrap(), &"ten");

        let values = sparse.iter().map(|e| e.value).collect::<Vec<_>>();
        assert_eq!(&values, &[&"zero", &"one", &"two", &"six", &"ten"]);
    }
}
