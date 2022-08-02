//! Provides tracking of modifications to component stores.
use std::marker::PhantomData;

use rustc_hash::FxHashSet;

use crate::{
    storage::{HasEntityInfo, HasId},
    system::current_iteration,
};

pub struct TrackedIter<I: Iterator>(u64, I, fn(&I::Item, u64) -> bool);

impl<I: Iterator> Iterator for TrackedIter<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let e = self.1.next()?;
            if self.2(&e, self.0) {
                return Some(e);
            }
        }
    }
}

// impl<'a, T, I: Iterator<Item = &'a Entry<T>>> IntoJoinIterator for
// TrackedIter<'a, T, I> {    type Iter = Self;
//
//    fn join_iter(self) -> Self::Iter {
//        self
//    }
//}

/// Tracks changed components in a store with components of `T`.
///
/// ## WARNING
/// Do not pass the store as `T`. `T` is the type of the component.
#[derive(Debug)]
pub struct Tracker<T> {
    last_changed: u64,
    id_cache: FxHashSet<usize>,
    _phantom: PhantomData<T>,
}

impl<T> Default for Tracker<T> {
    fn default() -> Self {
        Self {
            last_changed: 0,
            id_cache: FxHashSet::default(),
            _phantom: Default::default(),
        }
    }
}

impl<T: Send + Sync + 'static> Tracker<T> {
    pub fn last_update(&self) -> u64 {
        self.last_changed
    }

    /// Clear the tracker.
    pub fn clear(&mut self) {
        self.last_changed = current_iteration();
    }

    /// Return an iterator over all items in the store that
    /// have been changed or added since the last time this tracker was cleared.
    pub fn changed_iter<E: AsRef<T> + HasEntityInfo>(
        &self,
        store: impl IntoIterator<Item = E>,
    ) -> impl Iterator<Item = E> {
        TrackedIter(self.last_changed, store.into_iter(), |item, changed| {
            item.has_changed_since(changed)
        })
    }

    ///// Return a parallel iterator over all items in the store that
    ///// have been changed or added since the last time this tracker was cleared.
    // pub fn changed_par_iter<'b, S>(
    //    &self,
    //    store: S,
    //) -> impl ParallelIterator<Item = Option<&'b Entry<T>>> +
    //) IndexedParallelIterator
    // where
    //    S: IntoParallelIterator<Item = Option<&'b Entry<T>>>,
    //    <S as IntoParallelIterator>::Iter: IndexedParallelIterator,
    //{
    //    let last_changed = self.last_changed;
    //    store.into_par_iter().map(move |me| {
    //        let e = me?;
    //        if e.has_changed_since(last_changed) {
    //            Some(e)
    //        } else {
    //            None
    //        }
    //    })
    //}

    /// Return an iterator over all items in the store that
    /// have been added since the last time this tracker was cleared.
    pub fn added_iter<E: AsRef<T> + HasEntityInfo>(
        &self,
        store: impl IntoIterator<Item = E>,
    ) -> impl Iterator<Item = E> {
        TrackedIter(self.last_changed, store.into_iter(), |item, changed| {
            item.was_added_since(changed)
        })
    }

    ///// Return a parallel iterator over all items in the store that
    ///// have been added since the last time this tracker was cleared.
    // pub fn added_par_iter<'b, S>(
    //    &self,
    //    store: S,
    //) -> impl ParallelIterator<Item = Option<&'b Entry<T>>> +
    //) IndexedParallelIterator
    // where
    //    S: IntoParallelIterator<Item = Option<&'b Entry<T>>>,
    //    <S as IntoParallelIterator>::Iter: IndexedParallelIterator,
    //{
    //    let last_changed = self.last_changed;
    //    store.into_par_iter().map(move |me| {
    //        let e = me?;
    //        if e.has_changed_since(last_changed) && e.added {
    //            Some(e)
    //        } else {
    //            None
    //        }
    //    })
    //}

    /// Return an iterator over all items in the store that
    /// have been modified since the last time this tracker was cleared.
    ///
    /// Does not include items that have been added since the last time
    /// this tracker was cleared.
    pub fn modified_iter<E: AsRef<T> + HasEntityInfo>(
        &self,
        store: impl IntoIterator<Item = E>,
    ) -> impl Iterator<Item = E> {
        TrackedIter(self.last_changed, store.into_iter(), |item, changed| {
            item.was_modified_since(changed)
        })
    }

    ///// Return a parallel iterator over all items in the store that
    ///// have been modified since the last time this tracker was cleared.
    /////
    ///// Does not include items that have been added since the last time
    ///// this tracker was cleared.
    // pub fn modified_par_iter<'b, S>(
    //    &self,
    //    store: S,
    //) -> impl ParallelIterator<Item = Option<&'b Entry<T>>> +
    //) IndexedParallelIterator
    // where
    //    S: IntoParallelIterator<Item = Option<&'b Entry<T>>>,
    //    <S as IntoParallelIterator>::Iter: IndexedParallelIterator,
    //{
    //    let last_changed = self.last_changed;
    //    store.into_par_iter().map(move |me| {
    //        let e = me?;
    //        if e.has_changed_since(last_changed) && !e.added {
    //            Some(e)
    //        } else {
    //            None
    //        }
    //    })
    //}

    /// Return the ids of deleted items **since the last time this function was
    /// called**.
    ///
    /// ## Warning
    /// This does **not** return the deleted ids since the last time the tracker
    /// was cleared.
    pub fn deleted<E: AsRef<T> + HasId>(
        &mut self,
        store: impl IntoIterator<Item = E>,
    ) -> Vec<usize> {
        let old_cache = std::mem::replace(
            &mut self.id_cache,
            FxHashSet::from_iter(store.into_iter().map(|item| item.id())),
        );
        old_cache
            .difference(&self.id_cache)
            .map(|id| *id)
            .collect::<Vec<_>>()
    }
}

#[cfg(test)]
mod test {
    use crate::{
        anyhow,
        storage::separated::join::*,
        storage::{
            archetype::{AllArchetypes, Query},
            separated::*,
        },
        system::increment_current_iteration,
    };

    use super::*;

    fn sanity_check_deleted_vec() {
        let mut tracker: Tracker<f32> = Tracker::default();
        let mut store = VecStorage::<f32>::default();
        store.insert(0, 0.0);
        store.insert(1, 0.0);
        store.insert(2, 0.0);

        assert!(tracker.deleted(store.iter()).is_empty());
        assert!(tracker.deleted(store.iter()).is_empty());
        assert_eq!(
            rustc_hash::FxHashSet::from_iter(vec![0, 1, 2]),
            tracker.id_cache
        );

        let _ = store.remove(1);
        assert_eq!(
            vec![0, 2],
            store
                .iter()
                .into_iter()
                .map(|item| item.id())
                .collect::<Vec<_>>()
        );
        assert_eq!(vec![1], tracker.deleted(store.iter()));
        store.insert(3, 0.0);
        store.insert(4, 0.0);
        store.insert(5, 0.0);
        assert!(tracker.deleted(store.iter()).is_empty());
        store.remove(0);
        store.remove(2);
        store.remove(3);
        assert_eq!(vec![0, 2, 3], tracker.deleted(store.iter()));
    }

    fn sanity_check_added_vec() -> anyhow::Result<()> {
        let mut tracker: Tracker<f32> = Tracker::default();
        let mut store = VecStorage::<f32>::default();
        store.insert(0, 0.0);
        store.insert(1, 0.0);
        store.insert(2, 0.0);

        let changed = tracker
            .changed_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert_eq!(vec![0, 1, 2], changed);
        let added = tracker
            .added_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert_eq!(added, changed);

        increment_current_iteration();
        tracker.clear();

        let changed = tracker
            .changed_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert!(changed.is_empty());
        let added = tracker
            .added_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert!(added.is_empty());

        store.insert(3, 0.0);
        let changed = tracker
            .changed_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert_eq!(vec![3], changed);
        let added = tracker
            .added_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert_eq!(vec![3], added);
        let modified = tracker
            .modified_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert!(modified.is_empty());

        increment_current_iteration();
        tracker.clear();

        *store.get_mut(3).unwrap() = 1.0;
        let changed = tracker
            .changed_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert_eq!(vec![3], changed);
        let added = tracker
            .added_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert!(added.is_empty());
        let modified = tracker
            .modified_iter(store.iter())
            .map(|e| e.id())
            .collect::<Vec<_>>();
        assert_eq!(changed, modified);

        Ok(())
    }

    fn get_change_state<I>(iter: I) -> Vec<(usize, bool)>
    where
        I: Iterator,
        <I as Iterator>::Item: HasId + HasEntityInfo,
    {
        iter.map(|e| (e.id(), e.info().added)).collect()
    }

    fn sanity_check_vec() {
        let mut tracker = Tracker::<f32>::default();
        let mut store = VecStorage::<f32>::default();
        store.insert(0, 0.0);
        store.insert(1, 0.0);
        store.insert(2, 0.0);

        assert_eq!(
            vec![(0, true), (1, true), (2, true)],
            get_change_state(tracker.changed_iter(store.iter()))
        );

        let _ = increment_current_iteration() + 1;
        tracker.clear();
        assert!(get_change_state(tracker.changed_iter(store.iter())).is_empty());

        store.insert(1, 1.0);
        let changed = tracker.changed_iter(store.iter());
        assert_eq!(vec![(1, false)], get_change_state(changed));

        let changed = tracker
            .changed_iter(store.iter())
            .map(|e| (e.id(), *e.as_ref()))
            .collect::<Vec<_>>();
        assert_eq!(vec![(1, 1.0)], changed);

        store.insert(2, 2.0);

        increment_current_iteration();

        let changed = tracker
            .changed_iter(store.iter())
            .map(|e| (e.id(), *e.as_ref()))
            .collect::<Vec<_>>();
        assert_eq!(vec![(1, 1.0), (2, 2.0)], changed);

        tracker.clear();
        assert!(tracker.changed_iter(store.iter()).next().is_none());

        increment_current_iteration();

        // accessing a mutable component should mark it as changed
        *store.get_mut(2).unwrap() = 200.0;

        // inserting a component should mark it as changed
        store.insert(0, 0.0);

        assert_eq!(
            vec![(0, 0.0), (2, 200.0)],
            tracker
                .changed_iter(store.iter())
                .map(|e| (e.id(), *e.as_ref()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn sanity_check_separated() {
        sanity_check_vec();
        sanity_check_added_vec().unwrap();
        sanity_check_deleted_vec();
    }

    #[test]
    fn can_track_archetypes() {
        let mut tracker = Tracker::<f32>::default();
        let mut store = AllArchetypes::default();
        store.insert(0, 0.0f32);
        store.insert(1, 1.0f32);
        store.insert(2, 2.0f32);
        {
            let mut query = Query::<(&f32,)>::try_from(&mut store).unwrap();
            assert_eq!(
                vec![(0, 0.0), (1, 1.0), (2, 2.0)],
                query
                    .run()
                    .filter_map(|(f,)| if f.has_changed_since(tracker.last_update()) {
                        Some((f.id(), *f))
                    } else {
                        None
                    })
                    .collect::<Vec<_>>()
            );
        }
        increment_current_iteration();
        tracker.clear();
        store.unify_resources();
        *store.get_mut::<f32>(&1).unwrap() = 100.0;
        {
            let mut query = Query::<(&f32,)>::try_from(&mut store).unwrap();
            assert_eq!(
                vec![(1, 100.0)],
                query
                    .run()
                    .filter_map(|(f,)| if f.has_changed_since(tracker.last_update()) {
                        Some((f.id(), *f))
                    } else {
                        None
                    })
                    .collect::<Vec<_>>()
            );
        }
        increment_current_iteration();
        tracker.clear();
        store.unify_resources();
        store.remove_any(0);
        store.remove_any(2);
        {
            let mut query = Query::<(&f32,)>::try_from(&mut store).unwrap();
            assert!(
                query
                    .run()
                    .filter_map(|(f,)| if f.has_changed_since(tracker.last_update()) {
                        Some((f.id(), *f))
                    } else {
                        None
                    })
                    .collect::<Vec<_>>()
                    .is_empty()
            );
        }
    }

    #[test]
    fn can_join_tracked() {
        let tracker_a = Tracker::<f32>::default();
        let tracker_b = Tracker::<usize>::default();
        let mut store_a = VecStorage::<f32>::default();
        store_a.insert(0, 0.0);
        store_a.insert(1, 1.0);
        store_a.insert(2, 2.0);
        let mut store_b = VecStorage::<usize>::default();
        store_b.insert(0, 0);
        store_b.insert(2, 2);

        let _added: Vec<_> = (
            tracker_a.added_iter(&store_a),
            tracker_b.added_iter(&store_b),
        )
            .join()
            .collect();
    }
}
