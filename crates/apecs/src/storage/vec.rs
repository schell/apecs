//! Storage using a naive vector.
use std::slice::IterMut;

use crate::storage::*;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use super::Entry;

#[derive(Clone)]
pub struct VecStorage<T>(Vec<Option<Entry<T>>>);

impl<T> VecStorage<T> {
    pub fn new_with_capacity(cap: usize) -> Self {
        let mut store = Vec::new();
        store.resize_with(cap, Default::default);

        VecStorage(store)
    }
}

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

impl<T: Send + Sync + 'static> CanReadStorage for VecStorage<T> {
    type Component = T;
    type Iter<'a> = VecStorageIter<'a, T> where T: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        self.0.get(id).and_then(|m| m.as_ref().map(|e| &e.value))
    }

    fn iter(&self) -> Self::Iter<'_> {
        VecStorageIter(self.0.iter())
    }

    fn last(&self) -> Option<&Entry<Self::Component>> {
        let me = self.0.last()?;
        me.as_ref()
    }

    type ParIter<'a> = rayon::iter::Map<
        rayon::slice::Iter<'a, Option<Entry<T>>>,
        fn(&Option<Entry<T>>) -> Option<&Entry<T>>,
    >;

    fn par_iter(&self) -> Self::ParIter<'_> {
        self.into_par_iter()
    }
}

impl<T: Send + Sync + 'static> CanWriteStorage for VecStorage<T> {
    type IterMut<'a> = VecStorageIterMut<'a, T> where T: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        self.0
            .get_mut(id)
            .and_then(|m| m.as_mut().map(|e| e.deref_mut()))
    }

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component> {
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

    type ParIterMut<'a> = rayon::iter::Map<
        rayon::slice::IterMut<'a, Option<Entry<T>>>,
        fn(&'a mut Option<Entry<T>>) -> Option<&'a mut Entry<T>>,
    >;

    fn par_iter_mut(&mut self) -> Self::ParIterMut<'_> {
        self.into_par_iter()
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

impl<T: Send + Sync + 'static> WorldStorage for VecStorage<T> {
    fn new_with_capacity(cap: usize) -> Self {
        VecStorage::new_with_capacity(cap)
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
