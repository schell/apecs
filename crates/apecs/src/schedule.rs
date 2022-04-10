//! System scheduling for outer parallelism.
//!
//! This module contains trait definitions. Implementations can be found in other modeluse.
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::{iter::FlatMap, slice::Iter, sync::Arc};

use crate::{mpsc, world, FetchReadyResource, Resource, ResourceId};

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
    ) -> anyhow::Result<()>;
}

#[derive(Debug)]
pub(crate) struct BatchData<T> {
    pub resources: Vec<FxHashMap<ResourceId, FetchReadyResource>>,
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
            data.resources.push(ready_resources);
            data.loaned_resources.extend(loaned_resources);
        }

        Ok(data)
    }

    /// Run the batch in parallel and then unify the resources to make the world "whole".
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
        if parallelize {
            self.systems_mut()
                .par_iter_mut()
                .zip(data.resources.into_par_iter())
                .try_for_each(|(system, data)| -> anyhow::Result<()> {
                    system.run(resources_from_system.0.clone(), data)
                })?;
        } else {
            self.systems_mut()
                .iter_mut()
                .zip(data.resources.into_iter())
                .try_for_each(|(system, data)| -> anyhow::Result<()> {
                    system.run(resources_from_system.0.clone(), data)
                })?;
        }

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
        let deps:Vec<String> = vec![];
        self.add_system_with_dependecies(new_system, deps.into_iter())
    }

    fn add_system_with_dependecies(&mut self, new_system: Self::System, deps: impl Iterator<Item = impl AsRef<str>>) {
        let deps = rustc_hash::FxHashSet::from_iter(deps.map(|s| s.as_ref().to_string()));
        'batch_loop: for batch in self.batches_mut() {
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
        batch.add_system(new_system);
        self.add_batch(batch);
    }

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
        schedule.add_system(SyncSystem::new("one", |()| {Ok(())}));
        schedule.add_system_with_dependecies(SyncSystem::new("two", |()| {Ok(())}), ["one"].into_iter());

        let batches = schedule.batches();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].systems()[0].name(), "one");
        assert_eq!(batches[1].systems()[0].name(), "two");
    }
}
