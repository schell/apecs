//! Provides tracking of modifications to component stores.
use std::marker::PhantomData;

use hibitset::{AtomicBitSet, BitSet};
use rayon::iter::{IndexedParallelIterator, IntoParallelIterator, ParallelIterator};

use crate::{CanFetch, IsResource, ResourceId, Write};

use super::{CanReadStorage, CanWriteStorage, Entry, ParallelStorage, current_iteration};

#[derive(Default)]
pub struct Tracker<T> {
    last_changed: u64,
    _phantom: PhantomData<T>,
}

pub struct Tracked<T: IsResource> {
    tracker: Write<Tracker<T>>,
    storage: T,
}

impl<T: CanFetch + Send + Sync + 'static> CanFetch for Tracked<T> {
    fn reads() -> Vec<crate::ResourceId> {
        T::reads()
    }

    fn writes() -> Vec<crate::ResourceId> {
        let mut ws = T::writes();
        ws.extend(Write::<Tracker<T>>::writes());
        ws
    }

    fn construct(
        resource_return_tx: crate::mpsc::Sender<(crate::ResourceId, crate::Resource)>,
        fields: &mut rustc_hash::FxHashMap<crate::ResourceId, crate::FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        let tracker = Write::<Tracker<T>>::construct(resource_return_tx.clone(), fields)?;
        let storage = T::construct(resource_return_tx, fields)?;
        Ok(Tracked { tracker, storage })
    }
}

impl<S: IsResource + CanReadStorage> CanReadStorage for Tracked<S> {
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
}

impl<S: IsResource + CanWriteStorage> CanWriteStorage for Tracked<S> {
    type IterMut<'b> = S::IterMut<'b>
    where
        Self: 'b;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        self.storage.get_mut(id)
    }

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component> {
        self.storage.insert(id, component)
    }

    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        self.storage.remove(id)
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        self.storage.iter_mut()
    }
}

impl<S: IsResource + ParallelStorage + Default> ParallelStorage for Tracked<S>
where
    S::Component: Send + Sync,
{
    type ParIter<'b> = S::ParIter<'b>
    where
        Self: 'b;

    type IntoParIter<'b> = S::IntoParIter<'b>
    where Self: 'b;

    type ParIterMut<'b> = S::ParIterMut<'b>
    where
        Self: 'b;

    type IntoParIterMut<'b> = S::IntoParIterMut<'b>
        where Self: 'b;

    fn par_iter(&self) -> Self::IntoParIter<'_> {
        self.storage.par_iter()
    }

    fn par_iter_mut<'b>(&'b mut self) -> Self::IntoParIterMut<'b> {
        self.storage.par_iter_mut()
    }
}

impl<'a, T: IsResource + ParallelStorage + Default> IntoParallelIterator for &'a Tracked<T>
where
    T::Component: Send + Sync,
{
    type Iter = T::ParIter<'a>;

    type Item = Option<&'a Entry<T::Component>>;

    fn into_par_iter(self) -> Self::Iter {
        self.storage.par_iter().into_par_iter()
    }
}

impl<'a, T: IsResource + ParallelStorage + Default> IntoParallelIterator for &'a mut Tracked<T>
where
    T::Component: Send + Sync,
{
    type Iter = T::ParIterMut<'a>;

    type Item = Option<&'a mut Entry<T::Component>>;

    fn into_par_iter(self) -> Self::Iter {
        self.par_iter_mut().into_par_iter()
    }
}

impl<T: IsResource + CanReadStorage> Tracked<T> {
    pub fn changed(&self) -> impl Iterator<Item = &Entry<T::Component>> {
        let last_changed = self.tracker.last_changed;
        self.storage
            .iter()
            .filter(move |e| e.has_changed_since(last_changed))
    }
}

impl<T: IsResource + CanWriteStorage> Tracked<T> {
    pub fn changed_mut(&mut self) -> impl Iterator<Item = &mut Entry<T::Component>> {
        let last_changed = self.tracker.last_changed;
        self.storage
            .iter_mut()
            .filter(move |e| e.has_changed_since(last_changed))
    }
}

impl<T: IsResource + ParallelStorage> Tracked<T>
where
    T::Component: Send + Sync + 'static,
{
    pub fn changed_par(
        &self,
    ) -> impl ParallelIterator<Item = Option<&Entry<T::Component>>> + IndexedParallelIterator {
        self.storage.par_iter().into_par_iter().map(|me| {
            let e = me?;
            if e.has_changed_since(self.tracker.last_changed) {
                Some(e)
            } else {
                None
            }
        })
    }

    pub fn changed_par_mut(
        &mut self,
    ) -> impl ParallelIterator<Item = Option<&mut Entry<T::Component>>> + IndexedParallelIterator {
        self.storage.par_iter_mut().into_par_iter().map(|me| {
            let e = me?;
            if e.has_changed_since(self.tracker.last_changed) {
                Some(e)
            } else {
                None
            }
        })
    }
}

impl<T: IsResource> Tracked<T> {
    /// Clears modifications up to the current time.
    pub fn clear(&mut self) {
        self.tracker.last_changed = current_iteration();
    }
}

#[cfg(test)]
mod test {
    use crate::{storage::*, world::*};

    #[test]
    fn sanity() -> anyhow::Result<()> {
        let mut world = World::default();
        world
            .with_default_resource::<VecStorage<f32>>()?
            .with_default_resource::<Tracker<VecStorage<f32>>>()?;

        let mut tracked = world.fetch::<Tracked<Write<VecStorage<f32>>>>()?;
        tracked.insert(0, 0.0);
        tracked.insert(1, 0.0);
        tracked.insert(2, 0.0);

        increment_current_iteration();
        tracked.insert(1, 1.0);

        let changed = tracked
            .changed()
            .map(|e| (e.id(), *e.value()))
            .collect::<Vec<_>>();
        assert_eq!(vec![(1, 1.0)], changed);

        tracked.insert(2, 2.0);
        let changed = tracked
            .changed_par()
            .filter_map(|me| me.map(|e| (e.id(), *e.value())))
            .collect::<Vec<_>>();
        assert_eq!(vec![(1, 1.0), (2, 2.0)], changed);


        Ok(())
    }
}
