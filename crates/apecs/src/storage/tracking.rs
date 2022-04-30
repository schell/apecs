//! Provides tracking of modifications to component stores.
use std::marker::PhantomData;

use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use crate as apecs;
use crate::{CanFetch, IsResource, Read, Write};

use super::{current_iteration, CanReadStorage, Entry};

/// Tracks changed components in a store with components of `T`.
///
/// ## WARNING
/// Do not pass the store as `T`. `T` is the type of the component.
pub struct Tracker<T> {
    last_changed: u64,
    _phantom: PhantomData<T>,
}

impl<T> Default for Tracker<T> {
    fn default() -> Self {
        Self {
            last_changed: current_iteration(),
            _phantom: Default::default(),
        }
    }
}

impl<T: Send + Sync + 'static> Tracker<T> {
    pub fn changed_iter<'b>(
        &self,
        store: &'b impl CanReadStorage<Component = T>,
    ) -> impl Iterator<Item = &'b Entry<T>> {
        let last_changed = self.last_changed;
        store
            .iter()
            .filter(move |e| e.has_changed_since(last_changed))
    }

    pub fn changed_par_iter<'b>(
        &self,
        store: &'b impl CanReadStorage<Component = T>,
    ) -> impl ParallelIterator<Item = Option<&'b Entry<T>>> + IndexedParallelIterator {
        let last_changed = self.last_changed;
        store.par_iter().into_par_iter().map(move |me| {
            let e = me?;
            if e.has_changed_since(last_changed) {
                Some(e)
            } else {
                None
            }
        })
    }
}

#[derive(CanFetch)]
pub struct Tracked<T: CanReadStorage + Default + IsResource> {
    tracker: Write<Tracker<T::Component>>,
    storage: Read<T>,
}

impl<S: IsResource + Default + CanReadStorage> CanReadStorage for Tracked<S> {
    type Component = S::Component;

    type Iter<'b> = S::Iter<'b>
    where
        Self: 'b;

    fn last(&self) -> Option<&Entry<Self::Component>> {
        self.storage.last()
    }

    fn get(&self, id: usize) -> Option<&Self::Component> {
        self.storage.get(id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.storage.iter()
    }

    type ParIter<'b> = S::ParIter<'b>
    where
        Self: 'b;
    fn par_iter(&self) -> Self::ParIter<'_> {
        self.storage.par_iter()
    }
}

impl<'a, T: IsResource + Default + CanReadStorage> IntoParallelIterator for &'a Tracked<T>
where
    T::Component: Send + Sync,
{
    type Iter = T::ParIter<'a>;

    type Item = Option<&'a Entry<T::Component>>;

    fn into_par_iter(self) -> Self::Iter {
        self.storage.par_iter().into_par_iter()
    }
}

impl<T: IsResource + Default + CanReadStorage> Tracked<T> {
    pub fn changed(&self) -> impl Iterator<Item = &Entry<T::Component>> {
        self.tracker.changed_iter(&self.storage)
    }

    pub fn changed_par(
        &self,
    ) -> impl ParallelIterator<Item = Option<&Entry<T::Component>>> + IndexedParallelIterator {
        self.tracker.changed_par_iter(&self.storage)
    }

    /// Clears modifications up to the current iteration.
    pub fn clear_changes(&mut self) {
        self.tracker.last_changed = current_iteration();
    }
}

#[cfg(test)]
mod test {
    use crate::{
        storage::*,
        system::{end, ok},
        world::*,
    };

    #[test]
    fn sanity() -> anyhow::Result<()> {
        clear_iteration();

        let mut world = World::default();
        world
            .with_default_resource::<VecStorage<f32>>()?
            .with_default_resource::<Tracker<f32>>()?;

        {
            let mut store = world.fetch::<Write<VecStorage<f32>>>()?;
            store.insert(0, 0.0);
            store.insert(1, 0.0);
            store.insert(2, 0.0);

            increment_current_iteration();
            store.insert(1, 1.0);
        }

        {
            let tracked = world.fetch::<Tracked<VecStorage<f32>>>()?;
            let changed = tracked
                .changed()
                .map(|e| (e.id(), *e.value()))
                .collect::<Vec<_>>();
            assert_eq!(vec![(1, 1.0)], changed);
        }

        {
            let mut store = world.fetch::<Write<VecStorage<f32>>>()?;
            store.insert(2, 2.0);
        }

        increment_current_iteration();

        {
            let mut tracked = world.fetch::<Tracked<VecStorage<f32>>>()?;
            let changed = tracked
                .changed_par()
                .filter_map(|me| me.map(|e| (e.id(), *e.value())))
                .collect::<Vec<_>>();
            assert_eq!(vec![(1, 1.0), (2, 2.0)], changed);

            tracked.clear_changes();
            assert!(tracked.changed().next().is_none());
        }

        increment_current_iteration();

        {
            // accessing a mutable component should mark it as changed
            let mut store = world.fetch::<Write<VecStorage<f32>>>()?;
            let mut e = store.get_mut(2).unwrap();
            *e.deref_mut() = 200.0;

            // inserting a component should mark it as changed
            let _ = store.insert(0, 0.0);
        }

        assert_eq!(
            vec![(0, 0.0), (2, 200.0)],
            world
                .fetch::<Tracked<VecStorage<f32>>>()?
                .changed()
                .map(|e| (e.id(), *e.value()))
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn system_tracking() -> anyhow::Result<()> {
        struct Component(f32);

        impl StoredComponent for Component {
            type StorageType = VecStorage<Self>;
        }

        let mut mutate = true;
        let mut world = World::default();
        world
            .with_default_storage::<Component>()?
            .with_system("insert", |mut comps: WriteStore<Component>| {
                let _ = comps.insert(0, Component(0.0));
                let _ = comps.insert(1, Component(0.0));
                let _ = comps.insert(2, Component(0.0));

                end()
            })?
            .with_system_with_dependencies(
                "modify",
                &["insert"],
                move |mut comps: WriteStore<Component>| {
                    if mutate {
                        if let Some(c) = comps.get_mut(1) {
                            c.0 += 1.0;
                            mutate = false;
                        }
                    }

                    ok()
                },
            )?
            .with_system_with_dependencies(
                "check",
                &["modify"],
                |(mut comps, mut cache): (Tracked<VecStorage<Component>>, Write<Vec<usize>>)| {
                    for entry in comps.changed() {
                        cache.push(entry.id());
                    }

                    comps.clear_changes();

                    ok()
                },
            )?;

        world.tick_sync()?;
        assert_eq!(
            &vec![1],
            world.fetch::<Write<Vec<usize>>>().unwrap().deref(),
        );

        world.tick_sync()?;
        assert_eq!(
            &vec![1],
            world.fetch::<Write<Vec<usize>>>().unwrap().deref(),
        );

        Ok(())
    }
}
