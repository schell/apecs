//! Binary search tree storage.
use rayon::iter::IntoParallelIterator;
use std::{
    collections::btree_map::{BTreeMap, Iter, IterMut},
    sync::{Arc, Mutex},
};

use super::{
    CanReadStorage, CanWriteStorage, Entry, ParallelStorage, StorageIter, StorageIterGetFn,
    StorageIterMut, WorldStorage,
};

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
        if let Some(prev_id) = self.last_key.as_mut() {
            if *prev_id < id {
                *prev_id = id
            }
        } else {
            self.last_key = Some(id);
        }
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

impl<T: Send + Sync + 'static> ParallelStorage for BTreeStorage<T> {
    type ParIter<'a> = rayon::iter::MapWith<
        rayon::range::Iter<usize>,
        &'a Self,
        fn(&mut &'a Self, usize) -> Option<&'a T>,
    >;

    type IntoParIter<'a> = StorageIter<'a, Self>;

    type ParIterMut<'a> = rayon::iter::MapWith<
        rayon::range::Iter<usize>,
        Arc<Mutex<&'a mut Self>>,
        StorageIterGetFn<'a, Self>,
    >;

    type IntoParIterMut<'a> = StorageIterMut<'a, Self>;

    fn par_iter(&self) -> Self::IntoParIter<'_> {
        StorageIter(self)
    }

    fn par_iter_mut(&mut self) -> Self::IntoParIterMut<'_> {
        StorageIterMut(self)
    }
}

impl<T: Send + Sync + 'static> WorldStorage for BTreeStorage<T> {
    fn new_with_capacity(_: usize) -> Self {
        BTreeStorage::default()
    }
}
