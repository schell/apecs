//! Storage using a naive vector.
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::{
    ops::DerefMut,
    slice::IterMut
};

use crate::storage::{Entry, MaybeIter, MaybeParIter, Without};

#[derive(Clone)]
pub struct VecStorage<T>(Vec<Option<Entry<T>>>);

impl<T> Default for VecStorage<T> {
    fn default() -> Self {
        VecStorage(vec![])
    }
}

pub struct VecStorageIter<'a, T>(std::slice::Iter<'a, Option<Entry<T>>>);

impl<'a, T> VecStorageIter<'a, T> {
    pub fn new(vs: &'a VecStorage<T>) -> Self {
        VecStorageIter(vs.0.iter())
    }
}

impl<'a, T> Iterator for VecStorageIter<'a, T> {
    type Item = &'a Entry<T>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut item = self.0.next()?;
        while item.is_none() {
            item = self.0.next()?;
        }
        item.as_ref()
    }
}

impl<'a, T: Send + Sync> IntoParallelIterator for &'a VecStorage<T> {
    type Iter = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&Entry<T>>,
    >;

    type Item = Option<&'a Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        (&self.0).into_par_iter().map(|may| may.as_ref())
    }
}

impl<'a, T: Send + Sync + 'static> IntoIterator for &'a VecStorage<T> {
    type Item = &'a Entry<T>;

    type IntoIter = VecStorageIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        VecStorage::iter(self)
    }
}

pub struct VecStorageIterMut<'a, T>(IterMut<'a, Option<Entry<T>>>);

impl<'a, T> Iterator for VecStorageIterMut<'a, T> {
    type Item = &'a mut Entry<T>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut item: &mut Option<Entry<T>> = self.0.next()?;
        while item.is_none() {
            item = self.0.next()?;
        }
        item.as_mut()
    }
}

impl<'a, T> VecStorageIterMut<'a, T> {
    pub fn new(vs: &'a mut VecStorage<T>) -> Self {
        VecStorageIterMut(vs.0.iter_mut())
    }
}

impl<'a, T: Send> IntoParallelIterator for &'a mut VecStorage<T> {
    type Iter = rayon::iter::Map<
        rayon::slice::IterMut<'a, Option<Entry<T>>>,
        fn(&'a mut Option<Entry<T>>) -> Option<&'a mut Entry<T>>,
    >;

    type Item = Option<&'a mut Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.0.as_mut_slice().into_par_iter().map(|me| me.as_mut())
    }
}

impl<'a, T: Send + Sync + 'static> IntoIterator for &'a mut VecStorage<T> {
    type Item = &'a mut Entry<T>;

    type IntoIter = VecStorageIterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

pub type VecStorageParIter<'a, T> = rayon::iter::Map<
    rayon::slice::Iter<'a, Option<Entry<T>>>,
    fn(&Option<Entry<T>>) -> Option<&Entry<T>>,
>;

pub type VecStorageParIterMut<'a, T> = rayon::iter::Map<
    rayon::slice::IterMut<'a, Option<Entry<T>>>,
    fn(&mut Option<Entry<T>>) -> Option<&mut Entry<T>>,
>;

impl<T: Send + Sync + 'static> VecStorage<T> {
    pub fn get(&self, id: usize) -> Option<&T> {
        self.0.get(id).and_then(|m| m.as_ref().map(|e| &e.value))
    }

    pub fn iter(&self) -> VecStorageIter<'_, T> {
        VecStorageIter(self.0.iter())
    }

    pub fn last(&self) -> Option<&Entry<T>> {
        let me = self.0.last()?;
        me.as_ref()
    }

    pub fn par_iter(&self) -> VecStorageParIter<'_, T> {
        self.into_par_iter()
    }

    /// Return an iterator over all contiguous entities, regardless
    /// of whether they reside in the storage.
    ///
    /// ## WARNING
    /// This iterator is unconstrained. It does not terminate.
    pub fn maybe(&self) -> MaybeIter<&Entry<T>, VecStorageIter<'_, T>> {
        MaybeIter::new(self.iter())
    }

    /// Return an iterator over all contiguous entities, regardless
    /// of whether they reside in the storage. Uses mutable components.
    pub fn maybe_mut(&mut self) -> MaybeIter<&mut Entry<T>, VecStorageIterMut<'_, T>> {
        MaybeIter::new(self.iter_mut())
    }

    pub fn maybe_par_iter(&self) -> MaybeParIter<VecStorageParIter<'_, T>> {
        MaybeParIter(self.par_iter())
    }

    pub fn maybe_par_iter_mut(&mut self) -> MaybeParIter<VecStorageParIterMut<'_, T>> {
        MaybeParIter(self.par_iter_mut())
    }

    /// Get a mutable reference to the component with the given id.
    ///
    /// ## NOTE
    /// This will cause the component's entry to be marked as changed.
    pub fn get_mut(&mut self, id: usize) -> Option<&mut T> {
        let me = self.0.get_mut(id)?;
        let e = me.as_mut()?;
        Some(e.deref_mut())
    }

    pub fn insert(&mut self, id: usize, component: T) -> Option<T> {
        if id >= self.0.len() {
            self.0.resize_with(id + 1, Default::default);
        }

        if self.0[id].is_some() {
            self.0[id].as_mut().map(|e| e.replace_value(component))
        } else {
            self.0[id] = Some(Entry::new(id, component));
            None
        }
    }

    pub fn remove(&mut self, id: usize) -> Option<T> {
        if id < self.0.len() {
            self.0[id].take().map(|entry| entry.value)
        } else {
            None
        }
    }

    /// Return an iterator to all entries, mutably.
    ///
    /// ## NOTE
    /// Iterating over the entries mutably will _not_ cause the entries to be
    /// marked as changed. Only after mutating the underlying component
    /// through [`DerefMut`] or [`Entry::set_value`], etc, will a change be
    /// reflected in the entry.
    pub fn iter_mut(&mut self) -> VecStorageIterMut<'_, T> {
        VecStorageIterMut(self.0.iter_mut())
    }

    /// Return a parallel iterator to all entries, mutably.
    ///
    /// ## NOTE
    /// Iterating over the entries mutably will _not_ cause the entries to be
    /// marked as changed. Only after mutating the underlying component
    /// through [`DerefMut`] or [`Entry::set_value`], etc, will a change be
    /// reflected in the entry.
    pub fn par_iter_mut(&mut self) -> VecStorageParIterMut<'_, T> {
        self.0.as_mut_slice().into_par_iter().map(|me| me.as_mut())
    }

    /// Perform entity upkeep, defragmentation or any other upkeep on the
    /// storage. When the storage is added through
    /// [`SeparateStorageExt::with_storage`]
    /// or [`SeparateStorageExt::with_default_storage`], this function will be
    /// called once per frame, automatically.
    pub fn upkeep(&mut self, dead_ids: &[usize]) {
        for id in dead_ids {
            let _ = self.remove(*id);
        }
    }

    /// Efficient allocation for a known number of components.
    pub fn new_with_capacity(cap: usize) -> Self {
        let mut store = Vec::new();
        store.resize_with(cap, Default::default);

        VecStorage(store)
    }
}

impl<'a, T> std::ops::Not for &'a VecStorage<T> {
    type Output = Without<&'a VecStorage<T>>;

    fn not(self) -> Self::Output {
        Without(self)
    }
}

impl<'a, T> std::ops::Not for &'a mut VecStorage<T> {
    type Output = Without<&'a mut VecStorage<T>>;

    fn not(self) -> Self::Output {
        Without(self)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn vec_store_can_insert_and_remove() {
        let mut store = VecStorage::<usize>::new_with_capacity(10);
        let _ = store.insert(0, 0);
        let _ = store.insert(1, 1);
        let _ = store.insert(2, 2);

        assert!(store.remove(0).is_some());
        assert!(store.remove(1).is_some());
        assert!(store.remove(2).is_some());
        assert!(store.remove(0).is_none());

        let size = 10_000;
        let mut store = VecStorage::<usize>::new_with_capacity(size);
        for i in 0..size {
            let _ = store.insert(i, i);
        }

        for i in 0..size {
            assert!(store.remove(i).is_some(), "{} not in store", i);
        }
    }
}
