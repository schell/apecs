//! Provides a storage based on ranges of entities.
//!
//! ## Things to try to make it go faster:
//! - [x] vecs instead of vecdeque
//! - [x] custom iterators
//! - [ ] use smallvecs instead of vec
use std::marker::PhantomData;

use rayon::iter::IntoParallelIterator;

use super::{CanReadStorage, CanWriteStorage, Entry, ParallelStorage, WorldStorage};

pub struct MissingRange<'a, T>(usize, usize, PhantomData<&'a T>);

pub struct RangeStoreIter<'a, T> {
    group: Option<(usize, &'a [Entry<T>])>,
    // RangeStore elements, sorted backwards
    groups: Vec<(usize, &'a [Entry<T>])>,
}

impl<'a, T> Iterator for RangeStoreIter<'a, T> {
    type Item = Entry<&'a T>;

    fn next(&mut self) -> Option<Self::Item> {
        #[inline]
        fn next_in_group<'a, T>(index: &mut usize, group: &mut &'a [Entry<T>]) -> Option<Entry<&'a T>> {
            let id = *index;
            match std::mem::replace(group, &[]) {
                [entry, rest @ ..] => {
                    *index += 1;
                    *group = rest;
                    Some(entry.as_ref())
                }
                [] => None,
            }
        }

        if let Some((index, group)) = self.group.as_mut() {
            next_in_group(index, group)
        } else {
            let (mut start, mut next_group) = self.groups.pop()?;
            let entry = next_in_group(&mut start, &mut next_group);
            self.group = Some((start, next_group));
            entry
        }
    }
}

pub struct RangeStoreIterMut<'a, T> {
    group: Option<std::slice::IterMut<'a, Entry<T>>>,
    // RangeStore elements, sorted backwards
    groups: Vec<std::slice::IterMut<'a, Entry<T>>>,
}

impl<'a, T> Iterator for RangeStoreIterMut<'a, T> {
    type Item = Entry<&'a mut T>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(group) = self.group.as_mut() {
            if let Some(entry) = group.next() {
                return Some(entry.as_mut());
            }
        }

        if let Some(mut group) = self.groups.pop() {
            let entry = group.next().map(Entry::as_mut)?;
            self.group = Some(group);
            Some(entry)
        } else {
            None
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
        let mut value = value;
        // element goes last in the last group
        let new_end_group = if let (Some((start, end)), Some(group)) =
            (self.groups.last_mut(), self.elements.last_mut())
        {
            if *end + 1 == id {
                *end = id;
                group.push(Entry{key: id, value});
                return None;
            } else if *start <= id && id <= *end {
                std::mem::swap(&mut group[id - *start].value, &mut value);
                return Some(value);
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
                group.insert(0, Entry{key: id, value});
                return None;
            } else if *start <= id && id <= *end {
                std::mem::swap(&mut group[id - *start].value, &mut value);
                return Some(value);
            }

            id < *start
        } else {
            false
        };

        if new_end_group {
            // component goes first in a new group
            self.groups.push((id, id));
            let mut group = Vec::new();
            group.push(Entry{key: id, value});
            self.elements.push(group);
            return None;
        }

        if new_start_group {
            // component goes first in a new group
            self.groups.insert(0, (id, id));
            let mut group = Vec::new();
            group.push(Entry{key: id, value});
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
                group.insert(0, Entry{key: id, value});
                return None;
            }

            if id < *start {
                // insert a new group here
                let insert_at = n;

                self.groups.insert(insert_at, (id, id));
                self.elements.insert(insert_at, {
                    let mut group = Vec::default();
                    group.push(Entry{key: id, value});
                    group
                });
                return None;
            }

            if *end + 1 == id {
                *end = id;
                group.push(Entry{key: id, value});
                return None;
            }

            if *start <= id && id <= *end {
                std::mem::swap(&mut group[id - *start].value, &mut value);
                return Some(value);
            }
        }

        self.groups.push((id, id));
        self.elements.push({
            let mut group = Vec::default();
            group.push(Entry{key: id, value});
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
                return components.get_mut(id - *start).map(|e| e.as_mut().value);
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

    pub fn iter<'a>(&'a self) -> RangeStoreIter<'a, T> {
        RangeStoreIter {
            group: None,
            groups: self
                .groups
                .iter()
                .zip(self.elements.iter())
                .map(|((start, _), g)| (*start, g.as_slice()))
                .rev()
                .collect::<Vec<_>>(),
        }
    }

    pub fn iter_mut<'a>(
        &'a mut self,
    ) -> RangeStoreIterMut<'a, T> {
        RangeStoreIterMut {
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

//impl<T: Send + Sync + 'static> RangeStore<T> {
//    pub fn par_iter<'a>(
//        &'a self,
//    ) -> rayon::iter::Flatten<
//        rayon::vec::IntoIter<
//            rayon::iter::Chain<
//                rayon::iter::RepeatN<Option<&'a T>>,
//                rayon::iter::Map<
//                    rayon::slice::Iter<'a, T>,
//                    fn(&'a T) -> Option<&'a T>,
//                >,
//            >,
//        >,
//    > {
//        let (_, groups) = self.groups.iter().zip(self.elements.iter()).fold(
//            (0, vec![]),
//            |(prev_end, mut groups), ((start, end), group)| {
//                groups.push(par_group_iter(prev_end, *start, group));
//                (*end, groups)
//            },
//        );
//
//        groups.into_par_iter().flatten()
//    }
//
//    pub fn par_iter_mut<'a>(
//        &'a mut self,
//    ) -> rayon::iter::Flatten<
//        rayon::vec::IntoIter<
//            rayon::iter::Chain<
//                rayon::iter::Map<rayon::range::Iter<usize>, fn(usize) -> Option<&'a mut T>>,
//                rayon::iter::Map<
//                    rayon::slice::IterMut<'a, T>,
//                    fn(&'a mut T) -> Option<&'a mut T>,
//                >,
//            >,
//        >,
//    > {
//        let (_, groups) = self.groups.iter().zip(self.elements.iter_mut()).fold(
//            (0, vec![]),
//            |(prev_end, mut groups), ((start, end), group)| {
//                groups.push(par_group_iter_mut(prev_end, *start, group));
//                (*end, groups)
//            },
//        );
//
//        groups.into_par_iter().flatten()
//    }
//}

impl<T> CanReadStorage for RangeStore<T> {
    type Component = T;

    type Iter<'a> = RangeStoreIter<'a, T>
    where
        Self: 'a;

    fn last(&self) -> Option<super::Entry<&Self::Component>> {
        let group = self.elements.last()?;
        group.last().map(Entry::as_ref)
    }

    fn get(&self, id: usize) -> Option<&Self::Component> {
        RangeStore::get(self, id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        RangeStore::iter(self)
    }
}

impl<T> CanWriteStorage for RangeStore<T> {
    type IterMut<'a> = RangeStoreIterMut<'a, T>
    where
        Self: 'a;

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

impl<T: Send + Sync + 'static> ParallelStorage for RangeStore<T> {
    type ParIter<'a> = rayon::vec::IntoIter<Option<&'a T>>
    where
        Self: 'a;

    type IntoParIter<'a> = Self::ParIter<'a>
    where
        Self: 'a;

    type ParIterMut<'a> = rayon::vec::IntoIter<Option<&'a mut T>>
    where
        Self: 'a;

    type IntoParIterMut<'a> = Self::ParIterMut<'a>
    where
        Self: 'a;

    fn par_iter(&self) -> Self::IntoParIter<'_> {
        vec![].into_par_iter()
    }

    fn par_iter_mut(&mut self) -> Self::IntoParIterMut<'_> {
        vec![].into_par_iter()
    }
}

impl<T: Send + Sync + 'static> WorldStorage for RangeStore<T> {
    fn new_with_capacity(_cap: usize) -> Self {
        RangeStore::default()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn range_store_sanity() {
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

        fn collect(store: &RangeStore<usize>) -> Vec<((usize, usize), Vec<usize>)> {
            store
                .groups
                .iter()
                .map(|(s, e)| (*s, *e))
                .zip(
                    store
                        .elements
                        .iter()
                        .map(|vs| vs.iter().map(|e| e.value).collect::<Vec<_>>())
                        .collect::<Vec<_>>(),
                )
                .collect::<Vec<_>>()
        }

        let expected: Vec<((usize, usize), Vec<usize>)> = vec![
            ((0, 2), vec![0, 1, 2]),
            ((5, 5), vec![5]),
            ((9, 10), vec![9, 10]),
            ((12, 12), vec![12]),
        ];
        assert_eq!(expected, collect(&store));

        store.remove(5);
        assert_eq!(
            vec![
                ((0, 2), vec![0, 1, 2]),
                ((9, 10), vec![9, 10]),
                ((12, 12), vec![12]),
            ],
            collect(&store)
        );

        store.remove(0);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((9, 10), vec![9, 10]),
                ((12, 12), vec![12]),
            ],
            collect(&store)
        );

        store.remove(10);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            collect(&store)
        );

        store.insert(4, 4);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((4, 4), vec![4]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            collect(&store)
        );

        store.insert(3, 3);
        assert_eq!(
            vec![
                ((1, 2), vec![1, 2]),
                ((3, 4), vec![3, 4]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            collect(&store)
        );

        store.defrag();
        assert_eq!(
            vec![
                ((1, 4), vec![1, 2, 3, 4]),
                ((9, 9), vec![9]),
                ((12, 12), vec![12]),
            ],
            collect(&store)
        );
    }

    #[test]
    fn range_sanity() {
        let end_prev = 2;
        let start = 3;
        let range = (end_prev + 1)..start;
        assert_eq!(range.len(), 0);
    }
}
