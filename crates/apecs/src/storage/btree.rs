use rayon::iter::IntoParallelIterator;
use std::{
    collections::btree_map::{BTreeMap, Iter, IterMut},
    sync::{Arc, Mutex},
};

use super::{CanReadStorage, CanWriteStorage, Entry, StorageIter, StorageIterMut};

pub struct BTreeStorage<T> {
    inner: BTreeMap<usize, T>,
    last_key: Option<usize>,
}

impl<T> Default for BTreeStorage<T> {
    fn default() -> Self {
        Self {
            inner: BTreeMap::new(),
            last_key: None,
        }
    }
}

pub struct BTreeIter<'a, T>(Iter<'a, usize, T>);

impl<'a, T> Iterator for BTreeIter<'a, T> {
    type Item = Entry<&'a T>;

    fn next(&mut self) -> Option<Self::Item> {
        let (k, v) = self.0.next()?;
        Some(Entry { key: *k, value: v })
    }
}

impl<T> CanReadStorage for BTreeStorage<T> {
    type Component = T;

    type Iter<'a> = BTreeIter<'a, T>
    where
        Self: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        self.inner.get(&id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        BTreeIter(self.inner.iter())
    }

    fn last(&self) -> Option<Entry<&Self::Component>> {
        let key = self.last_key.as_ref()?;
        let value = self.inner.get(key)?;
        Some(Entry { key: *key, value })
    }
}

pub struct BTreeIterMut<'a, T>(IterMut<'a, usize, T>);

impl<'a, T> Iterator for BTreeIterMut<'a, T> {
    type Item = Entry<&'a mut T>;

    fn next(&mut self) -> Option<Self::Item> {
        let (k, v) = self.0.next()?;
        Some(Entry { key: *k, value: v })
    }
}

impl<T> CanWriteStorage for BTreeStorage<T> {
    type IterMut<'a> = BTreeIterMut<'a, T>
    where
        Self: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        self.inner.get_mut(&id)
    }

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component> {
        self.inner.insert(id, component)
    }

    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        self.inner.remove(&id)
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        BTreeIterMut(self.inner.iter_mut())
    }
}

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a BTreeStorage<T> {
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

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a mut BTreeStorage<T> {
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
