//! Storage using a naive vector.
use std::slice::{Iter, IterMut};

use crate::{join::{Without, WithoutIter}, storage::*};
use hibitset::BitSet;

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

pub struct VecStorageIter<'a, T>(Iter<'a, Option<Entry<T>>>);

impl<'a, T> Iterator for VecStorageIter<'a, T> {
    type Item = Entry<&'a T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.0.next()? {
                return Some(entry.as_ref());
            }
        }
    }
}

pub struct VecStorageIterMut<'a, T>(IterMut<'a, Option<Entry<T>>>);

impl<'a, T> Iterator for VecStorageIterMut<'a, T> {
    type Item = Entry<&'a mut T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.0.next()? {
                return Some(entry.as_mut());
            }
        }
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
        VecStorageIter(self.store.iter())
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
        VecStorageIterMut(self.store.iter_mut())
    }
}

impl<'a, T> std::ops::Not for &'a VecStorage<T> {
    type Output = WithoutIter<VecStorageIter<'a, T>>;

    fn not(self) -> Self::Output {
        WithoutIter::new(self.iter())
    }
}
