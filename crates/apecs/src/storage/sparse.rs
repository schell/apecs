//! Storage using a sparse set.
//!
//! ## Pros
//! * better than [`VecStorage`] on space if the storage actually is sparse.
//! * 0(1) insert and remove
//!
//! ## Cons
//! * non-monotonic inserts make the dense storage non-monotonic
//! * hard to parallelize
use std::{
    iter::Map,
    slice::{Iter, IterMut},
};

use crate::storage::*;

use super::Entry;

pub struct SparseStorage<T> {
    sparse: Vec<Option<usize>>,
    dense: Vec<Entry<T>>,
}

impl<T> Default for SparseStorage<T> {
    fn default() -> Self {
        Self {
            sparse: Default::default(),
            dense: Default::default(),
        }
    }
}

impl<T> CanReadStorage for SparseStorage<T> {
    type Component = T;

    type Iter<'a> = Map<Iter<'a, Entry<T>>, fn(&Entry<T>) -> Entry<&T>> where T: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        let dense_id = self.sparse.get(id)?.as_ref()?;
        Some(&self.dense.get(*dense_id)?.value)
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.dense.iter().map(Entry::as_ref)
    }

    fn last(&self) -> Option<Entry<&Self::Component>> {
        self.dense.last().map(Entry::as_ref)
    }
}

impl<T> CanWriteStorage for SparseStorage<T> {
    type IterMut<'a> = Map<IterMut<'a, Entry<T>>, fn(&mut Entry<T>) -> Entry<&mut T>> where T: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        let dense_id = self.dense_id(id)?;
        Some(&mut self.dense.get_mut(dense_id)?.value)
    }

    fn insert(&mut self, index: usize, value: T) -> Option<T> {
        if let Some(index) = self.dense_id(index) {
            let prev = std::mem::replace(&mut self.dense[index].value, value);
            Some(prev)
        } else {
            let mut dense_index = self.dense.len();
            if self.sparse.len() <= index {
                self.sparse.resize(index + 1, None);
            }
            while !self.dense.is_empty() && self.dense[dense_index - 1].key() > index {
                dense_index -= 1;
                self.sparse[self.dense[dense_index].key()] = Some(dense_index + 1);
            }
            self.sparse[index] = Some(dense_index);
            self.dense.insert(dense_index, Entry { key: index, value });
            None
        }
    }

    /// Removes the entry at the given key and returns it if
    /// possible.
    fn remove(&mut self, index: usize) -> Option<T> {
        if let Some(mut dense_index) = self.dense_id(index) {
            self.sparse[index] = None;
            let removed = self.dense.remove(dense_index);
            while dense_index < self.dense.len() {
                self.sparse[self.dense[dense_index].key()] = Some(dense_index);
                dense_index += 1;
            }
            Some(removed.value)
        } else {
            None
        }
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        self.dense.iter_mut().map(Entry::as_mut)
    }
}

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a SparseStorage<T> {
    type Iter = rayon::iter::MapWith<
        rayon::range::Iter<usize>,
        Self,
        fn(&mut Self, usize) -> Option<&'a T>,
    >;

    type Item = Option<&'a T>;

    fn into_par_iter(self) -> Self::Iter {
        StorageIter(self).into_par_iter()
    }
}

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a mut SparseStorage<T> {
    type Iter = rayon::iter::MapWith<
        rayon::range::Iter<usize>,
        Arc<Mutex<Self>>,
        fn(&mut Arc<Mutex<Self>>, usize) -> Option<&'a mut T>,
    >;

    type Item = Option<&'a mut T>;

    fn into_par_iter(self) -> Self::Iter {
        StorageIterMut(self).into_par_iter()
    }
}

impl<T> SparseStorage<T> {
    pub fn new_with_capacity(cap: usize) -> Self {
        let dense = vec![];
        let sparse = vec![None; cap];

        Self { dense, sparse }
    }

    pub(crate) fn dense_id(&self, id: usize) -> Option<usize> {
        let dense_id = self.sparse.get(id)?.as_ref()?;
        Some(*dense_id)
    }

    /// Returns an iterator over the dense keys in the order they
    /// were inserted or sorted.
    pub fn keys<'a>(&'a self) -> impl Iterator<Item = usize> + 'a {
        self.dense.iter().map(|e| e.key())
    }

    /// Returns an iterator over the sparse pointers.
    pub fn pointers<'a>(&'a self) -> impl Iterator<Item = &'a Option<usize>> {
        self.sparse.iter()
    }
}

impl<T: Send + Sync + 'static> WorldStorage for SparseStorage<T> {
    type ParIter<'a> = rayon::iter::MapWith<
            rayon::range::Iter<usize>,
        &'a Self,
        fn(&mut &'a Self, usize) -> Option<&'a T>,
        >;
    type IntoParIter<'a> = StorageIter<'a, Self>;

    type ParIterMut<'a> = rayon::iter::MapWith<
            rayon::range::Iter<usize>,
        Arc<Mutex<&'a mut Self>>,
        fn(&mut Arc<Mutex<&'a mut Self>>, usize) -> Option<&'a mut T>,
        >;
    type IntoParIterMut<'a> = StorageIterMut<'a, Self>;

    fn new_with_capacity(cap: usize) -> Self {
        SparseStorage::new_with_capacity(cap)
    }

    fn par_iter<'a>(&'a self) -> Self::IntoParIter<'a> {
        StorageIter(self)
    }

    fn par_iter_mut<'a>(&'a mut self) -> Self::IntoParIterMut<'a> {
        StorageIterMut(self)
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

        let pointers = sparse.pointers().take(4).collect::<Vec<_>>();
        assert_eq!(&pointers, &[&Some(0), &Some(1), &Some(2), &None]);

        sparse.insert(6, "six");

        let keys = sparse.keys().collect::<Vec<_>>();
        assert_eq!(&keys, &[0, 1, 2, 6, 10]);

        assert_eq!(sparse.get(0).unwrap(), &"zero");
        assert_eq!(sparse.get(1).unwrap(), &"one");
        assert_eq!(sparse.get(2).unwrap(), &"two");
        assert_eq!(sparse.get(6).unwrap(), &"six");
        assert_eq!(sparse.get(10).unwrap(), &"ten");

        let values = sparse.iter().map(|e| e.value).collect::<Vec<_>>();
        assert_eq!(&values, &[&"zero", &"one", &"two", &"six", &"ten"]);

        for n in 0..=10 {
            sparse.remove(n);
        }
    }

    #[test]
    pub fn spliterators() {
        let v = vec![0, 1, 2, 3, 4, 5, 6];
        let v_ref = v.as_slice();
        let (va, vb) = v_ref.split_at(3);
        assert!(va[0] == 0);
        assert!(vb[0] == 3);
    }
}
