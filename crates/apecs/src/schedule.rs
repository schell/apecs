//! System scheduling for outer parallelism.
//!
//! This module contains trait definitions. Implementations can be found in other modeluse.
use rayon::{iter::Either, prelude::*};
use rustc_hash::{FxHashMap, FxHashSet};
use std::{collections::VecDeque, iter::FlatMap, slice::Iter, sync::Arc};

use crate::{mpsc, system::ShouldContinue, world, FetchReadyResource, Resource, ResourceId, storage::increment_current_iteration};

pub(crate) trait IsBorrow: std::fmt::Debug {
    fn rez_id(&self) -> ResourceId;

    fn name(&self) -> &str;

    fn is_exclusive(&self) -> bool;
}

pub(crate) trait IsSystem: std::fmt::Debug {
    type Borrow: IsBorrow;

    fn name(&self) -> &str;

    fn borrows(&self) -> &[Self::Borrow];

    fn conflicts_with(&self, other: &impl IsSystem) -> bool {
        for borrow_here in self.borrows().iter() {
            for borrow_there in other.borrows() {
                if borrow_here.rez_id() == borrow_there.rez_id()
                    && (borrow_here.is_exclusive() || borrow_there.is_exclusive())
                {
                    return true;
                }
            }
        }
        false
    }

    fn run(
        &mut self,
        resource_return: mpsc::Sender<(ResourceId, Resource)>,
        resources: FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<ShouldContinue>;
}

#[derive(Debug)]
pub(crate) struct BatchData<T> {
    pub resources: VecDeque<FxHashMap<ResourceId, FetchReadyResource>>,
    pub loaned_resources: FxHashMap<ResourceId, Arc<Resource>>,
    pub extra: T,
}

impl<T> BatchData<T> {
    pub fn new(extra: T) -> Self {
        BatchData {
            resources: Default::default(),
            loaned_resources: Default::default(),
            extra,
        }
    }
}

/// A batch of systems that can run in parallel (because their borrows
/// don't conflict).
pub(crate) trait IsBatch: std::fmt::Debug + Default {
    type System: IsSystem + Send + Sync;
    type ExtraRunData: Clone;

    fn contains_system(&self, name: &str) -> bool {
        for system in self.systems() {
            if system.name() == name {
                return true;
            }
        }
        return false;
    }

    fn systems(&self) -> &[Self::System];

    fn systems_mut(&mut self) -> &mut [Self::System];

    fn trim_systems(&mut self, should_remove: FxHashSet<&str>);

    /// All borrows of all systems in this batch
    fn borrows(
        &self,
    ) -> FlatMap<
        Iter<Self::System>,
        &[<Self::System as IsSystem>::Borrow],
        fn(&Self::System) -> &[<Self::System as IsSystem>::Borrow],
    > {
        self.systems().iter().flat_map(|s| s.borrows())
    }

    fn add_system(&mut self, system: Self::System);

    /// Returns the barrier number this batch runs after.
    fn get_barrier(&self) -> usize;

    /// Sets the barrier this batch runs after.
    fn set_barrier(&mut self, barrier: usize);

    /// Prepare a vector of system data for each system in the batch given
    /// current world resources and previously loaned resources.
    fn prepare_batch_data(
        &self,
        resources: &mut FxHashMap<ResourceId, Resource>,
        extra: Self::ExtraRunData,
    ) -> anyhow::Result<BatchData<Self::ExtraRunData>> {
        let mut data = BatchData::new(extra);
        for system in self.systems() {
            let (ready_resources, loaned_resources) =
                world::try_take_resources(resources, system.borrows().iter(), Some(system.name()))?;
            data.resources.push_back(ready_resources);
            data.loaned_resources.extend(loaned_resources);
        }

        Ok(data)
    }

    fn take_systems(&mut self) -> Vec<Self::System>;

    fn set_systems(&mut self, systems: Vec<Self::System>);

    /// Run the batch in parallel and then unify the resources to make the world "whole".
    /// Return the names of the systems that should be trimmed.
    fn run(
        &mut self,
        parallelize: bool,
        mut data: BatchData<Self::ExtraRunData>,
        resources: &mut FxHashMap<ResourceId, Resource>,
        resources_from_system: &(
            mpsc::Sender<(ResourceId, Resource)>,
            mpsc::Receiver<(ResourceId, Resource)>,
        ),
    ) -> anyhow::Result<()> {
        let (remaining_systems, errs): (Vec<_>, Vec<_>) = if parallelize {
            self.take_systems()
                .into_par_iter()
                .zip(data.resources.into_par_iter())
                .filter_map(|(mut system, data)| {
                    match system.run(resources_from_system.0.clone(), data) {
                        Ok(ShouldContinue::Yes) => Some(Either::Left(system)),
                        Ok(ShouldContinue::No) => None,
                        Err(err) => Some(Either::Right(err)),
                    }
                })
                .partition_map(|e| e)
        } else {
            let mut remaining_systems = vec![];
            let mut errs = vec![];
            self.take_systems()
                .into_iter()
                .zip(data.resources.into_iter())
                .for_each(|(mut system, data)| {
                    match system.run(resources_from_system.0.clone(), data) {
                        Ok(ShouldContinue::Yes) => {
                            remaining_systems.push(system);
                        }
                        Ok(ShouldContinue::No) => {}
                        Err(err) => {
                            errs.push(err);
                        }
                    }
                });
            (remaining_systems, errs)
        };

        self.set_systems(remaining_systems);

        errs.into_iter()
            .fold(Ok(()), |may_err, err| match may_err {
                Ok(()) => Err(err),
                Err(prev) => Err(prev.context(format!("and {}", err))),
            })?;

        world::clear_returned_resources(
            Some("batch"),
            &resources_from_system.1,
            resources,
            &mut data.loaned_resources,
        )?;

        anyhow::ensure!(
            data.loaned_resources.is_empty(),
            "shared batch resources are still in the wild"
        );

        Ok(())
    }
}

pub(crate) trait IsSchedule: std::fmt::Debug {
    type System: IsSystem;
    type Batch: IsBatch<System = Self::System>;

    fn contains_system(&self, name: &str) -> bool {
        for batch in self.batches() {
            if batch.contains_system(name) {
                return true;
            }
        }
        false
    }

    fn batches_mut(&mut self) -> &mut [Self::Batch];

    fn batches(&self) -> &[Self::Batch];

    fn add_batch(&mut self, batch: Self::Batch);

    fn is_empty(&self) -> bool {
        for batch in self.batches() {
            if batch.systems().len() > 0 {
                return false;
            }
        }
        true
    }

    fn set_should_parallelize(&mut self, should: bool);

    fn get_should_parallelize(&self) -> bool;

    fn add_system(&mut self, new_system: Self::System) {
        let deps: Vec<String> = vec![];
        self.add_system_with_dependecies(new_system, deps.into_iter())
    }

    fn add_system_with_dependecies(
        &mut self,
        new_system: Self::System,
        deps: impl Iterator<Item = impl AsRef<str>>,
    ) {
        let deps = rustc_hash::FxHashSet::from_iter(deps.map(|s| s.as_ref().to_string()));
        let current_barrier = self.current_barrier();
        'batch_loop: for batch in self.batches_mut() {
            if batch.get_barrier() != current_barrier {
                continue;
            }
            for system in batch.systems() {
                if system.conflicts_with(&new_system) {
                    // the new system conflicts with one in
                    // the current batch, break and check
                    // the next batch
                    continue 'batch_loop;
                }
                if deps.contains(system.name()) {
                    // the new system has a dependency on this system,
                    // so it must be added to a later batch.
                    continue 'batch_loop;
                }
            }

            // the new system doesn't conflict with any existing
            // system, so add it to the batch
            batch.add_system(new_system);
            return;
        }

        // if the system hasn't found a compatible batch, make a new
        // batch and add it
        let mut batch = Self::Batch::default();
        batch.set_barrier(self.current_barrier());
        batch.add_system(new_system);
        self.add_batch(batch);
    }

    /// Returns the id of the last barrier.
    fn current_barrier(&self) -> usize;

    /// Inserts a barrier, any systems added after the barrier will run
    /// after the systems before the barrier.
    fn add_barrier(&mut self);

    fn run(
        &mut self,
        extra: <Self::Batch as IsBatch>::ExtraRunData,
        resources: &mut FxHashMap<ResourceId, Resource>,
        resources_from_system: &(
            mpsc::Sender<(ResourceId, Resource)>,
            mpsc::Receiver<(ResourceId, Resource)>,
        ),
    ) -> anyhow::Result<()> {
        let parallelize = self.get_should_parallelize();
        for batch in self.batches_mut() {
            increment_current_iteration();
            // make the batch data
            let batch_data = batch.prepare_batch_data(resources, extra.clone())?;
            batch.run(parallelize, batch_data, resources, resources_from_system)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Borrow {
    pub id: ResourceId,
    pub is_exclusive: bool,
}

impl IsBorrow for Borrow {
    fn rez_id(&self) -> ResourceId {
        self.id.clone()
    }

    fn name(&self) -> &str {
        self.id.name
    }

    fn is_exclusive(&self) -> bool {
        self.is_exclusive
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::system::*;

    #[test]
    fn schedule_with_dependencies() {
        let mut schedule = SyncSchedule::default();
        schedule.add_system(SyncSystem::new("one", |()| ok()));
        schedule
            .add_system_with_dependecies(SyncSystem::new("two", |()| ok()), ["one"].into_iter());

        let batches = schedule.batches();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].systems()[0].name(), "one");
        assert_eq!(batches[1].systems()[0].name(), "two");
    }

    #[test]
    fn schedule_with_barrier() {
        let mut schedule = SyncSchedule::default();
        schedule.add_system(SyncSystem::new("one", |()| ok()));
        schedule.add_barrier();
        schedule.add_system(SyncSystem::new("two", |()| ok()));
        schedule.add_system(SyncSystem::new("three", |()| ok()));
        schedule.add_barrier();
        schedule.add_system(SyncSystem::new("four", |()| ok()));

        let batches = schedule.batches();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].systems()[0].name(), "one");
        assert_eq!(batches[1].systems()[0].name(), "two");
        assert_eq!(batches[1].systems()[1].name(), "three");
        assert_eq!(batches[2].systems()[0].name(), "four");
    }

    #[test]
    fn schedule_with_ephemeral() {
        let mut schedule = SyncSchedule::default();
        schedule.add_system(SyncSystem::new("one", |()| end()));
        schedule.add_system(SyncSystem::new("two", |()| ok()));
        schedule.add_system(SyncSystem::new("three", |()| ok()));

        let batches = schedule.batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].systems().len(), 3);
        assert_eq!(batches[0].systems()[0].name(), "one");
        assert_eq!(batches[0].systems()[1].name(), "two");
        assert_eq!(batches[0].systems()[2].name(), "three");

        let mut resources = FxHashMap::default();
        let chan = mpsc::unbounded();
        schedule.run((), &mut resources, &chan).unwrap();

        let batches = schedule.batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].systems().len(), 2);
        assert_eq!(batches[0].systems()[0].name(), "two");
        assert_eq!(batches[0].systems()[1].name(), "three");
    }
}
