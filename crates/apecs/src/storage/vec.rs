//! Storage using a naive vector.
use crate::{storage::*, Write};
use hibitset::BitSet;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use super::Entry;

pub struct VecStorage<T> {
    mask: BitSet,
    store: Vec<Option<Entry<T>>>,
}

impl<T> VecStorage<T> {
    pub fn new_with_capacity(cap: usize) -> Self {
        let mut store = Vec::new();
        store.resize_with(cap, Default::default);

        VecStorage {
            mask: BitSet::new(),
            store,
        }
    }
}

impl<T> Default for VecStorage<T> {
    fn default() -> Self {
        VecStorage {
            mask: BitSet::new(),
            store: vec![],
        }
    }
}

pub struct VecStorageIter<'a, T>(usize, &'a [Option<Entry<T>>]);

impl<'a, T> Iterator for VecStorageIter<'a, T> {
    type Item = Entry<&'a T>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.0 < self.1.len() {
            let item = &self.1[self.0];
            self.0 += 1;
            if item.is_some() {
                return item.as_ref().map(Entry::as_ref);
            }
        }
        None
    }
}

impl<'a, T: Send + Sync> IntoParallelIterator for VecStorageIter<'a, T> {
    type Iter = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&T>,
    >;

    type Item = Option<&'a T>;

    fn into_par_iter(self) -> Self::Iter {
        self.1
            .into_par_iter()
            .map(|may| may.as_ref().map(|e| &e.value))
    }
}

impl<'a, T: Send + Sync> IntoParallelIterator for &'a VecStorage<T> {
    type Iter = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&T>,
    >;

    type Item = Option<&'a T>;

    fn into_par_iter(self) -> Self::Iter {
        self.iter().into_par_iter()
    }
}

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a Write<VecStorage<T>> {
    type Iter = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&T>,
    >;

    type Item = Option<&'a T>;

    fn into_par_iter(self) -> Self::Iter {
        self.iter().into_par_iter()
    }
}

pub struct VecStorageIterMut<'a, T>(&'a mut [Option<Entry<T>>]);

impl<'a, T> Iterator for VecStorageIterMut<'a, T> {
    type Item = Entry<&'a mut T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let slice = std::mem::replace(&mut self.0, &mut []);
            match slice {
                [] => break,
                [may_item, rest @ ..] => {
                    self.0 = rest;
                    match may_item {
                        Some(item) => return Some(item.as_mut()),
                        None => continue,
                    }
                }
            }
        }
        None
    }
}

impl<'a, T: Send> IntoParallelIterator for VecStorageIterMut<'a, T> {
    type Iter = rayon::iter::Map<
        rayon::slice::IterMut<'a, Option<Entry<T>>>,
        fn(&mut Option<Entry<T>>) -> Option<&mut T>,
    >;

    type Item = Option<&'a mut T>;

    fn into_par_iter(self) -> Self::Iter {
        self.0
            .into_par_iter()
            .map(|me| me.as_mut().map(|e| &mut e.value))
    }
}

impl<'a, T: Send> IntoParallelIterator for &'a mut VecStorage<T> {
    type Iter = rayon::iter::Map<
        rayon::slice::IterMut<'a, Option<Entry<T>>>,
        fn(&mut Option<Entry<T>>) -> Option<&mut T>,
    >;

    type Item = Option<&'a mut T>;

    fn into_par_iter(self) -> Self::Iter {
        self.iter_mut().into_par_iter()
    }
}

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a mut Write<VecStorage<T>> {
    type Iter = rayon::iter::Map<
        rayon::slice::IterMut<'a, Option<Entry<T>>>,
        fn(&mut Option<Entry<T>>) -> Option<&mut T>,
    >;

    type Item = Option<&'a mut T>;

    fn into_par_iter(self) -> Self::Iter {
        self.iter_mut().into_par_iter()
    }
}

impl<T> CanReadStorage for VecStorage<T> {
    type Component = T;
    type Iter<'a> = VecStorageIter<'a, T> where T: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        if self.mask.contains(id as u32) {
            self.store
                .get(id)
                .map(|m| m.as_ref().map(|e| &e.value))
                .flatten()
        } else {
            None
        }
    }

    fn iter(&self) -> Self::Iter<'_> {
        VecStorageIter(0, &self.store)
    }

    fn last(&self) -> Option<Entry<&Self::Component>> {
        let me = self.store.last()?;
        me.as_ref().map(Entry::as_ref)
    }
}

impl<T> CanWriteStorage for VecStorage<T> {
    type IterMut<'a> = VecStorageIterMut<'a, T> where T: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        if self.mask.contains(id as u32) {
            self.store
                .get_mut(id)
                .map(|m| m.as_mut().map(|e| &mut e.value))
                .flatten()
        } else {
            None
        }
    }

    fn insert(&mut self, id: usize, mut component: Self::Component) -> Option<Self::Component> {
        if self.mask.contains(id as u32) {
            std::mem::swap(&mut self.store[id].as_mut().unwrap().value, &mut component);
            return Some(component);
        }
        if id >= self.store.len() {
            self.store.resize_with(id + 1, Default::default);
        }
        self.mask.add(id as u32);
        self.store[id] = Some(Entry {
            key: id,
            value: component,
        });
        None
    }

    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        if self.mask.contains(id as u32) {
            self.mask.remove(id as u32);
            if let Some(e) = self.store[id].take() {
                return Some(e.value);
            }
        }
        None
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        VecStorageIterMut(&mut self.store)
    }
}

impl<'a, T> std::ops::Not for &'a VecStorage<T> {
    type Output = WithoutIter<VecStorageIter<'a, T>>;

    fn not(self) -> Self::Output {
        WithoutIter::new(self.iter())
    }
}
