//! Provides a storage based on ranges of entities.
//!
//! ## Things to try to make it go faster:
//! - [x] vecs instead of vecdeque
//! - [x] custom iterators
//! - [ ] use smallvecs instead of vec
use std::{marker::PhantomData, ops::DerefMut};

use rayon::iter::{
    plumbing::{Producer, Reducer},
    IndexedParallelIterator, IntoParallelIterator, ParallelIterator,
};

use crate::storage::{
    Entry,
    separate::{CanReadStorage, CanWriteStorage, Without, WorldStorage},
};

pub struct MissingRange<'a, T>(usize, PhantomData<&'a T>);

// pub struct RangeStoreIter<'a, T> {
//    group: Option<(usize, &'a [Entry<T>])>,
//    // RangeStore elements, sorted backwards
//    groups: Vec<(usize, &'a [Entry<T>])>,
//}

// impl<'a, T> Iterator for RangeStoreIter<'a, T> {
//    type Item = &'a Entry<T>;
//
//    fn next(&mut self) -> Option<Self::Item> {
//        #[inline]
//        fn next_in_group<'a, T>(
//            index: &mut usize,
//            group: &mut &'a [Entry<T>],
//        ) -> Option<&'a Entry<T>> {
//            match std::mem::replace(group, &[]) {
//                [entry, rest @ ..] => {
//                    *index += 1;
//                    *group = rest;
//                    Some(entry)
//                }
//                [] => None,
//            }
//        }
//
//        if let Some((index, group)) = self.group.as_mut() {
//            next_in_group(index, group)
//        } else {
//            let (mut start, mut next_group) = self.groups.pop()?;
//            let entry = next_in_group(&mut start, &mut next_group);
//            self.group = Some((start, next_group));
//            entry
//        }
//    }
//}

pub struct RangeStoreIter<I> {
    group: Option<I>,
    // vector of element iterators, the vector is sorted backwards
    // for efficient pop
    groups: Vec<I>,
}

impl<I: Iterator> Iterator for RangeStoreIter<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(group) = self.group.as_mut() {
            if let Some(entry) = group.next() {
                return Some(entry);
            }
        }

        if let Some(mut group) = self.groups.pop() {
            let entry = group.next()?;
            self.group = Some(group);
            Some(entry)
        } else {
            None
        }
    }
}

pub trait Splittable: Iterator + Sized {
    type ParIter: IndexedParallelIterator + ParallelIterator<Item = Self::Item>;

    fn len(&self) -> usize;
    fn split_at(self, index: usize) -> (Self, Self);
    fn into_indexed_par_iter(self) -> Self::ParIter;
}

impl<'a, T: Send + Sync> Splittable for std::slice::Iter<'a, T> {
    type ParIter = rayon::slice::Iter<'a, T>;

    fn len(&self) -> usize {
        ExactSizeIterator::len(self)
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.as_slice().split_at(index);
        (left.into_iter(), right.into_iter())
    }

    fn into_indexed_par_iter(self) -> Self::ParIter {
        self.as_slice().into_par_iter()
    }
}

impl<'a, T: Send + Sync> Splittable for std::slice::IterMut<'a, T> {
    type ParIter = rayon::slice::IterMut<'a, T>;

    fn len(&self) -> usize {
        ExactSizeIterator::len(self)
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.into_slice().split_at_mut(index);
        (left.into_iter(), right.into_iter())
    }

    fn into_indexed_par_iter(self) -> Self::ParIter {
        self.into_slice().into_par_iter()
    }
}

pub enum ParGroupIter<I> {
    Missing(usize),
    Present(I),
}

impl<I: Splittable> ParGroupIter<I> {
    pub fn len(&self) -> usize {
        match self {
            ParGroupIter::Missing(l) => *l,
            ParGroupIter::Present(i) => i.len(),
        }
    }
}

pub struct RangeStoreParIter<I>(Vec<ParGroupIter<I>>);

impl<I: Splittable> RangeStoreParIter<I> {
    fn len(&self) -> usize {
        self.0.iter().fold(0, |acc, group| acc + group.len())
    }
}

impl<I> ParallelIterator for RangeStoreParIter<I>
where
    I: Splittable + Send + Sync,
    I::Item: Send + Sync,
{
    type Item = Option<<I as Iterator>::Item>;

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: rayon::iter::plumbing::UnindexedConsumer<Self::Item>,
    {
        rayon::iter::plumbing::bridge(self, consumer)
    }
}

impl<I> IndexedParallelIterator for RangeStoreParIter<I>
where
    I: Splittable + Send + Sync,
    I::Item: Send + Sync,
{
    fn len(&self) -> usize {
        RangeStoreParIter::len(self)
    }

    fn drive<C: rayon::iter::plumbing::Consumer<Self::Item>>(mut self, consumer: C) -> C::Result {
        let starting_len = self.len();
        if let Some(tail) = self.0.pop() {
            let (left, right, reducer) = consumer.split_at(starting_len - tail.len());
            let (a, b) = rayon::join(
                || self.drive(left),
                || match tail {
                    ParGroupIter::Missing(length) => {
                        (0..length).into_par_iter().map(|_| None).drive(right)
                    }
                    ParGroupIter::Present(slice) => {
                        slice.into_indexed_par_iter().map(|e| Some(e)).drive(right)
                    }
                },
            );
            reducer.reduce(a, b)
        } else {
            rayon::iter::empty().drive(consumer)
        }
    }

    fn with_producer<CB: rayon::iter::plumbing::ProducerCallback<Self::Item>>(
        self,
        callback: CB,
    ) -> CB::Output {
        return callback.callback(RangeStoreProducer(self));

        struct RangeStoreProducer<I>(RangeStoreParIter<I>);

        impl<'a, I> Producer for RangeStoreProducer<I>
        where
            I: Splittable + Send + Sync,
            I::Item: Send + Sync,
        {
            type Item = Option<I::Item>;

            type IntoIter = std::vec::IntoIter<Self::Item>;

            fn into_iter(self) -> Self::IntoIter {
                let mut vs = vec![];
                for group in self.0 .0.into_iter() {
                    match group {
                        ParGroupIter::Missing(length) => {
                            for _ in 0..length {
                                vs.push(None);
                            }
                        }
                        ParGroupIter::Present(slice) => {
                            for entry in slice {
                                vs.push(Some(entry));
                            }
                        }
                    }
                }
                vs.into_iter()
            }

            fn split_at(self, index: usize) -> (Self, Self) {
                let mut num_items = index;
                let mut left = vec![];
                let mut right = vec![];
                let total_len = self.0.len();
                let mut groups = self.0 .0.into_iter();
                while let Some(group) = groups.next() {
                    let length = group.len();
                    if length > num_items {
                        // split the group
                        match group {
                            ParGroupIter::Missing(len) => {
                                left.push(ParGroupIter::Missing(num_items));
                                right.push(ParGroupIter::Missing(len - num_items));
                            }
                            ParGroupIter::Present(slice) => {
                                let (slice_left, slice_right) = slice.split_at(num_items);
                                left.push(ParGroupIter::Present(slice_left));
                                right.push(ParGroupIter::Present(slice_right));
                            }
                        }
                        // push the rest into right
                        right.extend(groups);
                        break;
                    } else if length <= num_items {
                        left.push(group);
                        num_items -= length;
                    }
                }

                let left = RangeStoreProducer(RangeStoreParIter(left));
                let right = RangeStoreProducer(RangeStoreParIter(right));
                debug_assert!(
                    left.0.len() == index,
                    "left producer wrong length, {} /= {}",
                    left.0.len(),
                    index
                );
                debug_assert!(
                    left.0.len() + right.0.len() == total_len,
                    "right producer wrong length, {} /= {}",
                    right.0.len(),
                    total_len - left.0.len()
                );

                (left, right)
            }
        }
    }
}

pub struct RangeStore<T> {
    groups: Vec<(usize, usize)>,
    elements: Vec<Vec<Entry<T>>>,
}

impl<T> Default for RangeStore<T> {
    fn default() -> Self {
        Self {
            groups: Default::default(),
            elements: Default::default(),
        }
    }
}

impl<T> RangeStore<T> {
    pub fn insert(&mut self, id: usize, value: T) -> Option<T> {
        // element goes last in the last group
        let new_end_group = if let (Some((start, end)), Some(group)) =
            (self.groups.last_mut(), self.elements.last_mut())
        {
            if *end + 1 == id {
                *end = id;
                group.push(Entry::new(id, value));
                return None;
            } else if *start <= id && id <= *end {
                return Some(group[id - *start].replace_value(value));
            }

            id > *end
        } else {
            false
        };

        // element goes first in the first group
        let new_start_group = if let (Some((start, end)), Some(group)) =
            (self.groups.first_mut(), self.elements.first_mut())
        {
            if id + 1 == *start {
                self.groups[0].0 = id;
                group.insert(0, Entry::new(id, value));
                return None;
            } else if *start <= id && id <= *end {
                return Some(group[id - *start].replace_value(value));
            }

            id < *start
        } else {
            false
        };

        if new_end_group {
            // component goes first in a new group
            self.groups.push((id, id));
            let mut group = Vec::new();
            group.push(Entry::new(id, value));
            self.elements.push(group);
            return None;
        }

        if new_start_group {
            // component goes first in a new group
            self.groups.insert(0, (id, id));
            let mut group = Vec::new();
            group.push(Entry::new(id, value));
            self.elements.insert(0, group);
            return None;
        }

        // search for the group it might belong to
        // we skip the front and back because we already checked those
        for (((start, end), n), group) in self
            .groups
            .iter_mut()
            .zip(0..)
            .skip(1)
            .zip(self.elements.iter_mut().skip(1))
        {
            if id + 1 == *start {
                *start = id;
                group.insert(0, Entry::new(id, value));
                return None;
            }

            if id < *start {
                // insert a new group here
                let insert_at = n;

                self.groups.insert(insert_at, (id, id));
                self.elements.insert(insert_at, {
                    let mut group = Vec::default();
                    group.push(Entry::new(id, value));
                    group
                });
                return None;
            }

            if *end + 1 == id {
                *end = id;
                group.push(Entry::new(id, value));
                return None;
            }

            if *start <= id && id <= *end {
                return Some(group[id - *start].replace_value(value));
            }
        }

        self.groups.push((id, id));
        self.elements.push({
            let mut group = Vec::default();
            group.push(Entry::new(id, value));
            group
        });

        None
    }

    pub fn get(&self, id: usize) -> Option<&T> {
        for ((start, end), components) in self.groups.iter().zip(self.elements.iter()) {
            if *start <= id && id <= *end {
                return components.get(id - *start).map(|e| &e.value);
            }
        }
        None
    }

    pub fn get_mut(&mut self, id: usize) -> Option<&mut T> {
        for ((start, end), components) in self.groups.iter_mut().zip(self.elements.iter_mut()) {
            if *start <= id && id <= *end {
                return components.get_mut(id - *start).map(|e| e.deref_mut());
            }
        }
        None
    }

    pub fn contains(&self, id: usize) -> bool {
        for (start, end) in self.groups.iter() {
            if id >= *start && id <= *end {
                return true;
            }
        }
        false
    }

    /// Pushes a component onto the end of the store and returns the id.
    pub fn push(&mut self, value: T) -> usize {
        if let Some((_start, end)) = self.groups.last_mut() {
            let id = *end + 1;
            *end = id;

            self.elements
                .last_mut()
                .unwrap()
                .push(Entry::new(id, value));

            id
        } else {
            self.groups.push((0, 0));
            self.elements.push(vec![Entry::new(0, value)]);
            0
        }
    }

    /// Removes the last component from the end of the store and returns it,
    /// if possible.
    pub fn pop(&mut self) -> Option<T> {
        if let Some((start, end)) = self.groups.last_mut() {
            if start < end {
                *end -= 1;
                return self
                    .elements
                    .last_mut()
                    .unwrap()
                    .pop()
                    .map(Entry::into_inner);
            }
        };
        let _ = self.groups.pop()?;
        let mut group = self.elements.pop()?;
        group.pop().map(Entry::into_inner)
    }

    pub fn remove(&mut self, id: usize) -> Option<T> {
        let mut new_group = None;
        let mut remove_group = None;
        let mut item = None;
        for ((start, end), (group, n)) in self
            .groups
            .iter_mut()
            .zip(self.elements.iter_mut().zip(0..))
        {
            if id >= *start && id <= *end {
                // we have to split this group
                let local_index = if id > *start { id - *start } else { 0 };
                let mut removed = group.split_off(local_index);
                let removed_end = *end;
                if !removed.is_empty() {
                    item = Some(removed.remove(0));
                }
                if group.is_empty() {
                    if removed.is_empty() {
                        remove_group = Some(n);
                    } else {
                        *start = id + 1;
                        *group = removed;
                        return item.map(|e| e.value);
                    }
                } else {
                    *end = if id == 0 { 0 } else { id - 1 };
                    if group.len() == 1 {
                        *start = *end;
                    }
                }

                if !removed.is_empty() {
                    new_group = Some((n + 1, removed_end, removed));
                }
                break;
            }
        }

        if let Some(n) = remove_group {
            self.groups.remove(n);
            self.elements.remove(n);
        }

        if let Some((n, end, group)) = new_group {
            self.groups.insert(n, (id + 1, end));
            self.elements.insert(n, group);
        }

        item.map(|e| e.value)
    }

    /// Defragment the inner memory layout
    pub fn defrag(&mut self) {
        self.defrag_partial(self.groups.len())
    }

    /// Defragment the inner memory layout by performing `n` merge operations.
    pub fn defrag_partial(&mut self, mut n: usize) {
        fn get_next_defrag_pair<T>(store: &RangeStore<T>) -> Option<(usize, usize)> {
            for (((_, a_end), (b_start, _)), i) in store
                .groups
                .iter()
                .zip(store.groups.iter().skip(1))
                .zip(0..)
            {
                if *a_end + 1 == *b_start {
                    return Some((i, i + 1));
                }
            }
            None
        }
        while let Some((a, b)) = get_next_defrag_pair(&self) {
            if n == 0 {
                break;
            }
            self.merge(a, b).unwrap();
            n -= 1;
        }
    }

    /// Merge groups a and b that are neighbors
    fn merge(&mut self, a: usize, b: usize) -> Option<()> {
        debug_assert!(a + 1 == b);
        debug_assert!(self.groups.len() > b);
        let (b_start, b_end) = self.groups.remove(b);
        let (_a_start, a_end) = self.groups.get_mut(a)?;
        debug_assert!(*a_end + 1 == b_start);
        *a_end = b_end;
        let b_group = self.elements.remove(b);
        let a_group = self.elements.get_mut(a)?;
        a_group.extend(b_group);
        Some(())
    }

    pub fn iter<'a>(&'a self) -> RangeStoreIter<std::slice::Iter<'a, Entry<T>>> {
        RangeStoreIter {
            group: None,
            groups: self
                .elements
                .iter()
                .map(|group| group.iter())
                .rev()
                .collect::<Vec<_>>(),
        }
    }

    pub fn iter_mut<'a>(&'a mut self) -> RangeStoreIter<std::slice::IterMut<'a, Entry<T>>> {
        RangeStoreIter {
            group: None,
            groups: self
                .elements
                .iter_mut()
                .map(|group| group.iter_mut())
                .rev()
                .collect::<Vec<_>>(),
        }
    }
}

impl<T: Clone> RangeStore<T> {
    /// Handy for debugging.
    pub fn groups(&self) -> Vec<((usize, usize), Vec<T>)> {
        self.groups
            .iter()
            .map(|(s, e)| (*s, *e))
            .zip(
                self.elements
                    .iter()
                    .map(|vs| vs.iter().map(|e| e.value.clone()).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
            )
            .collect::<Vec<_>>()
    }
}

impl<T: Send + Sync> CanReadStorage for RangeStore<T> {
    type Component = T;

    type Iter<'a> = RangeStoreIter<std::slice::Iter<'a, Entry<T>>>
    where
        Self: 'a;
    type ParIter<'a> = RangeStoreParIter<std::slice::Iter<'a, Entry<T>>>
    where
        Self: 'a;

    fn par_iter(&self) -> Self::ParIter<'_> {
        let mut next_id = 0;
        let mut groups = vec![];
        for ((start, end), group) in self.groups.iter().zip(self.elements.iter()) {
            if next_id < *start {
                groups.push(ParGroupIter::Missing(start - next_id));
            }

            groups.push(ParGroupIter::Present(group.iter()));
            next_id = *end + 1;
        }

        RangeStoreParIter(groups)
    }

    fn last(&self) -> Option<&super::Entry<Self::Component>> {
        let group = self.elements.last()?;
        group.last()
    }

    fn get(&self, id: usize) -> Option<&Self::Component> {
        RangeStore::get(self, id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        RangeStore::iter(self)
    }
}

impl<T: Send + Sync> CanWriteStorage for RangeStore<T> {
    type IterMut<'a> = RangeStoreIter<std::slice::IterMut<'a, Entry<T>>>
    where
        Self: 'a;

    type ParIterMut<'a> = RangeStoreParIter<std::slice::IterMut<'a, Entry<T>>>
    where
        Self: 'a;

    fn par_iter_mut(&mut self) -> Self::ParIterMut<'_> {
        let mut next_id = 0;
        let mut groups = vec![];
        for ((start, end), group) in self.groups.iter().zip(self.elements.iter_mut()) {
            if next_id < *start {
                groups.push(ParGroupIter::Missing(start - next_id));
            }

            groups.push(ParGroupIter::Present(group.iter_mut()));
            next_id = *end + 1;
        }

        RangeStoreParIter(groups)
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        RangeStore::get_mut(self, id)
    }

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component> {
        RangeStore::insert(self, id, component)
    }

    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        RangeStore::remove(self, id)
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        RangeStore::iter_mut(self)
    }
}

impl<T: Send + Sync + 'static> WorldStorage for RangeStore<T> {
    fn new_with_capacity(_cap: usize) -> Self {
        RangeStore::default()
    }
}

impl<'a, T> std::ops::Not for &'a RangeStore<T> {
    type Output = Without<&'a RangeStore<T>>;

    fn not(self) -> Self::Output {
        Without(self)
    }
}

impl<'a, T> std::ops::Not for &'a mut RangeStore<T> {
    type Output = Without<&'a mut RangeStore<T>>;

    fn not(self) -> Self::Output {
        Without(self)
    }
}

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a RangeStore<T> {
    type Iter = RangeStoreParIter<std::slice::Iter<'a, Entry<T>>>;

    type Item = Option<&'a Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter()
    }
}

impl<'a, T: Send + Sync + 'static> IntoParallelIterator for &'a mut RangeStore<T> {
    type Iter = RangeStoreParIter<std::slice::IterMut<'a, Entry<T>>>;

    type Item = Option<&'a mut Entry<T>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter_mut()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn sanity() {
        let mut store = RangeStore::<usize>::default();
        store.insert(0, 0);
        assert_eq!(store.get(0), Some(&0));

        store.insert(1, 1);
        store.insert(2, 2);
        assert_eq!(store.get(2), Some(&2));

        store.insert(10, 10);
        store.insert(11, 11);
        store.insert(12, 12);
        assert_eq!(store.get(12), Some(&12));
        assert_eq!(store.get(13), None);
        assert_eq!(store.groups.len(), 2);
        assert_eq!(store.groups[0], (0, 2));
        assert_eq!(store.groups[1], (10, 12));

        store.insert(9, 9);
        assert_eq!(store.get(9), Some(&9));
        assert_eq!(store.groups[1], (9, 12));

        store.insert(5, 5);
        assert_eq!(store.get(5), Some(&5));
        assert_eq!(store.groups.len(), 3);
        assert_eq!(store.groups[1], (5, 5));

        *store.get_mut(9).unwrap() = 999;
        assert_eq!(store.get(9), Some(&999));
        *store.get_mut(9).unwrap() = 9;

        let removed = store.remove(11);
        assert_eq!(11, removed.unwrap());
        assert_eq!(4, store.groups.len());

        let expected: Vec<((usize, usize), Vec<usize>)> = vec![
            ((0, 2), vec![0, 1, 2]),
            ((5, 5), vec![5]),
            ((9, 10), vec![9, 10]),
            ((12, 12), vec![12]),
        ];
        assert_eq!(expected, store.groups());

        store.remove(5);
        assert_eq!(
            vec![
                ((0, 2), vec![0, 1, 2]),
                ((9, 10), vec![9, 10]),
                ((12, 12), vec![12]),
            ],
            store.groups()
        );

        store.remove(0);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((9, 10), vec![9, 10]),
                ((12, 12), vec![12]),
            ],
            store.groups()
        );

        store.remove(10);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            store.groups()
        );

        store.insert(4, 4);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((4, 4), vec![4]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            store.groups()
        );

        store.insert(3, 3);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((3, 4), vec![3, 4]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            store.groups()
        );

        store.defrag();
        assert_eq!(
            vec![
                ((1, 4), vec![1, 2, 3, 4]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            store.groups()
        );

        store.remove(3);
        store.remove(4);
        store.remove(9);
        store.remove(12);
        store.insert(0, 0);
        assert_eq!(vec![((0, 2), vec![0, 1, 2]),], store.groups());

        store.remove(1);
        assert_eq!(vec![((0, 0), vec![0]), ((2, 2), vec![2]),], store.groups());
    }

    #[test]
    fn can_remove() {
        let mut store = RangeStore::<u32>::default();
        store.insert(0, 0);
        store.insert(1, 1);
        store.insert(2, 2);
        assert_eq!(vec![((0, 2), vec![0, 1, 2])], store.groups());
        store.remove(1);
        assert_eq!(vec![((0, 0), vec![0]), ((2, 2), vec![2]),], store.groups());
        assert_eq!(
            vec![0, 2],
            store.iter_mut().map(|e| e.id()).collect::<Vec<_>>()
        )
    }
}
