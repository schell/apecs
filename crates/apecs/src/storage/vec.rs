//! Storage using a naive vector.
use std::slice::IterMut;

use crate::storage::*;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use super::Entry;

pub struct VecStorage<T> (Vec<Option<Entry<T>>>);

impl<T> VecStorage<T> {
    pub fn new_with_capacity(cap: usize) -> Self {
        let mut store = Vec::new();
        store.resize_with(cap, Default::default);

        VecStorage (store)
    }
}

impl<T> Default for VecStorage<T> {
    fn default() -> Self {
        VecStorage (vec![])
    }
}

pub struct VecStorageIter<'a, T>(std::slice::Iter<'a, Option<Entry<T>>>);

impl<'a, T> VecStorageIter<'a, T> {
    pub fn new(vs: &'a VecStorage<T>) -> Self {
        VecStorageIter(vs.0.iter())
    }
}

impl<'a, T> Iterator for VecStorageIter<'a, T> {
    type Item = Entry<&'a T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let item = self.0.next()?;
            if let Some(item) = item {
                return Some(item.as_ref());
            }
        }
    }
}

impl<'a, T: Send + Sync> IntoParallelIterator for &'a VecStorage<T> {
    type Iter = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&T>,
    >;

    type Item = Option<&'a T>;

    fn into_par_iter(self) -> Self::Iter {
        (&self.0)
            .into_par_iter()
            .map(|may| may.as_ref().map(|e| &e.value))
    }
}

pub struct VecStorageIterMut<'a, T>(IterMut<'a, Option<Entry<T>>>);

impl<'a, T> Iterator for VecStorageIterMut<'a, T> {
    type Item = Entry<&'a mut T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.0.next()? {
                Some(entry) => return Some(entry.as_mut()),
                None => {}
            }
        }
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
        fn(&'a mut Option<Entry<T>>) -> Option<&'a mut T>,
    >;

    type Item = Option<&'a mut T>;

    fn into_par_iter(self) -> Self::Iter {
        self.0
            .as_mut_slice()
            .into_par_iter()
            .map(|me| me.as_mut().map(|e| &mut e.value))
    }
}

impl<T> CanReadStorage for VecStorage<T> {
    type Component = T;
    type Iter<'a> = VecStorageIter<'a, T> where T: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        self.0
            .get(id)
            .and_then(|m| m.as_ref().map(|e| &e.value))
    }

    fn iter(&self) -> Self::Iter<'_> {
        VecStorageIter(self.0.iter())
    }

    fn last(&self) -> Option<Entry<&Self::Component>> {
        let me = self.0.last()?;
        me.as_ref().map(Entry::as_ref)
    }
}

impl<T> CanWriteStorage for VecStorage<T> {
    type IterMut<'a> = VecStorageIterMut<'a, T> where T: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
            self.0
                .get_mut(id)
                .and_then(|m| m.as_mut().map(|e| &mut e.value))
    }

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component> {
        if id >= self.0.len() {
            self.0.resize_with(id + 1, Default::default);
        }

        let prev = std::mem::replace(&mut self.0[id], Some(Entry {
            key: id,
            value: component,
        }));

        prev.map(|entry| entry.value)
    }

    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        if id < self.0.len() {
            self.0[id].take().map(|entry| entry.value)
        } else {
            None
        }
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        VecStorageIterMut(self.0.iter_mut())
    }
}

impl<'a, T> std::ops::Not for &'a VecStorage<T> {
    type Output = WithoutIter<VecStorageIter<'a, T>>;

    fn not(self) -> Self::Output {
        WithoutIter::new(self.iter())
    }
}

impl<T: Send + Sync + 'static> ParallelStorage for VecStorage<T> {
    type ParIter<'a> = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&T>,
    >;
    type IntoParIter<'a> = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&T>,
    >;
    type ParIterMut<'a> = rayon::iter::Map<
        rayon::slice::IterMut<'a, Option<Entry<T>>>,
        fn(&'a mut Option<Entry<T>>) -> Option<&'a mut T>,
    >;
    type IntoParIterMut<'a> = rayon::iter::Map<
        rayon::slice::IterMut<'a, Option<Entry<T>>>,
        fn(&'a mut Option<Entry<T>>) -> Option<&'a mut T>,
    >;

    fn par_iter(&self) -> Self::IntoParIter<'_> {
        self.into_par_iter()
    }

    fn par_iter_mut(&mut self) -> Self::IntoParIterMut<'_> {
        self.into_par_iter()
    }
}

impl<T: Send + Sync + 'static> WorldStorage for VecStorage<T> {
    fn new_with_capacity(cap: usize) -> Self {
        VecStorage::new_with_capacity(cap)
    }
}
