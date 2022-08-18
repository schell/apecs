//! System scheduling for outer parallelism.
//!
//! This module contains trait definitions. Implementations can be found in
//! other modeluse.
use rustc_hash::{FxHashMap, FxHashSet};
use std::{collections::VecDeque, iter::FlatMap, slice::Iter, sync::Arc, any::Any};

use crate::{
    resource_manager::{ResourceManager, LoanManager},
    system::ShouldContinue,
    FetchReadyResource, Resource, ResourceId,
};

pub type UntypedSystemData = Box<dyn Any + Send + Sync>;

pub trait IsSystem: std::fmt::Debug {
    fn name(&self) -> &str;

    fn borrows(&self) -> &[Borrow];

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

    fn prep(&self, loan_mngr: &mut LoanManager<'_>) -> anyhow::Result<UntypedSystemData>;

    fn run(
        &mut self,
        data: UntypedSystemData,
    ) -> anyhow::Result<ShouldContinue>;
}

#[derive(Debug)]
pub struct BatchData<T> {
    // exclusive resources borrowed by at most one system in the batch
    pub resources: VecDeque<FxHashMap<ResourceId, FetchReadyResource>>,
    // unexclusive resources borrowed by at least one system in the batch
    pub borrowed_resources: FxHashMap<ResourceId, Arc<Resource>>,
    // any extra data needed by the batch to run
    pub extra: T,
}

impl<T> BatchData<T> {
    pub fn new(extra: T) -> Self {
        BatchData {
            resources: Default::default(),
            borrowed_resources: Default::default(),
            extra,
        }
    }
}

/// A batch of systems that can run in parallel (because their borrows
/// don't conflict).
pub trait IsBatch: std::fmt::Debug + Default {
    type System: IsSystem + Send + Sync;
    type ExtraRunData: Send + Sync + Clone;

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
    fn borrows(&self) -> FlatMap<Iter<Self::System>, &[Borrow], fn(&Self::System) -> &[Borrow]> {
        self.systems().iter().flat_map(|s| s.borrows())
    }

    fn add_system(&mut self, system: Self::System);

    /// Returns the barrier number this batch runs after.
    fn get_barrier(&self) -> usize;

    /// Sets the barrier this batch runs after.
    fn set_barrier(&mut self, barrier: usize);

    fn take_systems(&mut self) -> Vec<Self::System>;

    fn set_systems(&mut self, systems: Vec<Self::System>);

    /// Run the batch (possibly in parallel) and then unify the resources and make the world
    /// "whole".
    ///
    /// ## Note
    /// `parallelism` is roughly the number of threads to use for parallel operations.
    fn run(
        &mut self,
        parallelism: u32,
        _: Self::ExtraRunData,
        resource_manager: &mut ResourceManager,
    ) -> anyhow::Result<()>;
}

pub trait IsSchedule: std::fmt::Debug {
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

    fn batches_mut(&mut self) -> &mut Vec<Self::Batch>;

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

    fn set_parallelism(&mut self, threads: u32);

    fn get_parallelism(&self) -> u32;

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
        resource_manager: &mut ResourceManager,
    ) -> anyhow::Result<()> {
        resource_manager.unify_resources("IsSchedule::run before all")?;
        let parallelism = self.get_parallelism();
        self.batches_mut().retain(|batch| !batch.systems().is_empty());
        for batch in self.batches_mut() {
            batch.run(parallelism, extra.clone(), resource_manager)?;
            resource_manager.unify_resources("IsSchedule::run after one")?;
        }

        Ok(())
    }

    fn get_execution_order(&self) -> Vec<&str> {
        self.batches()
            .iter()
            .flat_map(|batch| {
                batch
                    .systems()
                    .iter()
                    .map(IsSystem::name)
                    .chain(vec!["---"])
            })
            .collect::<Vec<_>>()
    }
}

#[derive(Clone, Debug)]
pub struct Borrow {
    pub id: ResourceId,
    pub is_exclusive: bool,
}

impl Borrow {
    pub fn rez_id(&self) -> ResourceId {
        self.id.clone()
    }

    pub fn name(&self) -> &str {
        self.id.name
    }

    pub fn is_exclusive(&self) -> bool {
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

        let mut manager = ResourceManager::default();
        schedule.run((), &mut manager).unwrap();

        let batches = schedule.batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].systems().len(), 2);
        assert_eq!(batches[0].systems()[0].name(), "two");
        assert_eq!(batches[0].systems()[1].name(), "three");
    }
}
