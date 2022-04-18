//! Provides tracking of insert, remove and modify storage operations.
//!
//! TODO
//! - [ ] try generation-counter changed detection instead of bitmasks:
//!       https://github.com/bevyengine/bevy/pull/1471
use std::marker::PhantomData;

use hibitset::{AtomicBitSet, BitSet};
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use super::{CanReadStorage, CanWriteStorage, Entry, ParallelStorage};

#[derive(Default)]
pub struct Tracker<T> {
    inserted: BitSet,
    modified: AtomicBitSet,
    removed: BitSet,
    _component: PhantomData<T>,
}

impl<T> Tracker<T> {
    pub fn track<'a, Store>(&'a mut self, storage: &'a mut Store) -> TrackedStorage<'a, Store>
    where
        Store: CanWriteStorage<Component = T> + ParallelStorage,
    {
        TrackedStorage {
            tracker: self,
            storage,
        }
    }
}

pub struct TrackedStorage<'a, S: CanReadStorage> {
    pub(crate) tracker: &'a mut Tracker<S::Component>,
    pub(crate) storage: &'a mut S,
}

impl<'a, S: CanReadStorage> CanReadStorage for TrackedStorage<'a, S> {
    type Component = S::Component;

    type Iter<'b> = S::Iter<'b>
    where
        Self: 'b;

    fn last(&self) -> Option<Entry<&Self::Component>> {
        self.storage.last()
    }

    fn get(&self, id: usize) -> Option<&Self::Component> {
        self.storage.get(id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.storage.iter()
    }
}

// TODO: Use separate iterators for par modified and regular (atomicbitset is slower)
pub struct TrackedStorageIter<'a, S: CanWriteStorage + 'a>(&'a AtomicBitSet, S::IterMut<'a>);

impl<'a, S: CanWriteStorage> Iterator for TrackedStorageIter<'a, S> {
    type Item = <S::IterMut<'a> as Iterator>::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.1.next()?;
        self.0.add_atomic(entry.key() as u32);
        Some(entry)
    }
}

impl<'a, S: CanWriteStorage> CanWriteStorage for TrackedStorage<'a, S> {
    type IterMut<'b> = TrackedStorageIter<'b, S>
    where
        Self: 'b;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        let t = self.storage.get_mut(id)?;
        let _ = self.tracker.modified.add(id as u32);
        Some(t)
    }

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component> {
        let _ = self.tracker.inserted.add(id as u32);
        self.storage.insert(id, component)
    }

    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        let t = self.storage.remove(id)?;
        self.tracker.removed.add(id as u32);
        Some(t)
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        TrackedStorageIter(&self.tracker.modified, self.storage.iter_mut())
    }
}

pub type TrackedStorageGetMutFn<'b, S> = fn(
    &mut &'b AtomicBitSet,
    (Option<&'b mut <S as CanReadStorage>::Component>, usize),
) -> Option<&'b mut <S as CanReadStorage>::Component>;

impl<'store, S: ParallelStorage + Default + 'store> ParallelStorage for TrackedStorage<'store, S>
where
    S::Component: Send + Sync,
{
    type ParIter<'b> = S::ParIter<'b>
    where
        Self: 'b;

    type IntoParIter<'b> = S::IntoParIter<'b>
    where Self: 'b;

    type ParIterMut<'b> = rayon::iter::MapWith<
            rayon::iter::Zip<
                S::ParIterMut<'b>,
                rayon::range::Iter<usize>,
            >,
            &'b AtomicBitSet,
            TrackedStorageGetMutFn<'b, S>
        >
    where
        Self: 'b;

    type IntoParIterMut<'b> = rayon::iter::MapWith<
            rayon::iter::Zip<S::ParIterMut<'b>, rayon::range::Iter<usize>>,
        &'b AtomicBitSet,
        TrackedStorageGetMutFn<'b, S>,
        >
        where Self: 'b;

    fn par_iter(&self) -> Self::IntoParIter<'_> {
        self.storage.par_iter()
    }

    fn par_iter_mut<'b>(&'b mut self) -> Self::IntoParIterMut<'b> {
        let range = 0..self.storage.len();
        let par_iter_mut: S::ParIterMut<'b> = self.storage.par_iter_mut().into_par_iter();

        fn get_mut<'a, T>(
            modified: &mut &AtomicBitSet,
            (item, n): (Option<&'a mut T>, usize),
        ) -> Option<&'a mut T> {
            if item.is_some() {
                modified.add_atomic(n as u32);
            }
            item
        }

        par_iter_mut.zip(range).map_with(
            &self.tracker.modified,
            get_mut as TrackedStorageGetMutFn<'b, S>,
        )
    }
}

impl<'a, 'b: 'a, T: ParallelStorage + Default> IntoParallelIterator for &'a TrackedStorage<'b, T>
    where
    T::Component: Send + Sync
{
    type Iter = <TrackedStorage<'b, T> as ParallelStorage>::ParIter<'a>;

    type Item = Option<&'a T::Component>;

    fn into_par_iter(self) -> Self::Iter {
        self.storage.par_iter().into_par_iter()
    }
}

impl<'a, 'b: 'a, T: ParallelStorage + Default> IntoParallelIterator for &'a mut TrackedStorage<'b, T>
where
    T::Component: Send + Sync
{
    type Iter = <TrackedStorage<'b, T> as ParallelStorage>::ParIterMut<'a>;

    type Item = Option<&'a mut T::Component>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter_mut().into_par_iter()
    }
}
