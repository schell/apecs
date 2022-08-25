//! System scheduling for outer parallelism.
//!
//! This module contains trait definitions. Implementations can be found in
//! other modeluse.
use rustc_hash::FxHashSet;
use std::{any::Any, cmp::Ordering, iter::FlatMap, slice::Iter};

use super::{
    resource_manager::{LoanManager, ResourceManager},
    system::ShouldContinue,
    ResourceId,
};

use self::solver::SolverSystem;

pub type UntypedSystemData = Box<dyn Any + Send + Sync>;

pub trait IsSystem: std::fmt::Debug {
    fn name(&self) -> &str;

    fn borrows(&self) -> &[Borrow];

    fn dependencies(&self) -> &[Dependency];

    fn set_barrier(&mut self, barrier: usize);

    fn barrier(&self) -> usize;

    fn prep(&self, loan_mngr: &mut LoanManager<'_>) -> anyhow::Result<UntypedSystemData>;

    fn run(&mut self, data: UntypedSystemData) -> anyhow::Result<ShouldContinue>;
}

/// A batch of systems that can run in parallel (because their borrows
/// don't conflict).
pub(crate) trait IsBatch: std::fmt::Debug + Default {
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

    /// Run the batch (possibly in parallel) and then unify the resources and
    /// make the world "whole".
    ///
    /// ## Note
    /// `parallelism` is roughly the number of threads to use for parallel
    /// operations.
    fn run(
        &mut self,
        parallelism: u32,
        _: Self::ExtraRunData,
        resource_manager: &mut ResourceManager,
    ) -> anyhow::Result<()>;
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

    fn add_system(&mut self, mut new_system: Self::System) {
        new_system.set_barrier(self.current_barrier());
        let batches = std::mem::take(self.batches_mut());
        let mut systems = batches
            .into_iter()
            .flat_map(|mut batch| batch.take_systems())
            .collect::<Vec<_>>();
        systems.push(new_system);

        let solver_systems = systems
            .iter()
            .map(|sys| SolverSystem {
                name: sys.name().to_string(),
                dependencies: sys.dependencies().to_vec(),
                borrows: sys.borrows().to_vec(),
                barrier: sys.barrier(),
            })
            .collect::<Vec<_>>();

        let indices = solver::solve_order(&solver_systems).unwrap();
        debug_assert_eq!(indices.len(), systems.len());
        let mut indexed_systems = indices
            .into_iter()
            .zip(systems.into_iter())
            .collect::<Vec<_>>();
        indexed_systems.sort_by(|a, b| match a.0.total_cmp(&b.0) {
            Ordering::Equal => a.1.name().cmp(b.1.name()),
            o => o,
        });
        log::trace!(
            "pre-schedule: {:#?}",
            indexed_systems
                .iter()
                .map(|(i, sys)| (i, sys.name()))
                .collect::<Vec<_>>()
        );

        let mut batch = Self::Batch::default();
        let mut current_index = indexed_systems.first().map(|(i, _)| *i).unwrap_or(0.0);

        for (index, system) in indexed_systems.into_iter() {
            let batch_borrows = batch.borrows().cloned().collect::<Vec<_>>();
            if index > current_index || borrows_conflict(system.borrows(), &batch_borrows) {
                if !batch.systems().is_empty() {
                    self.add_batch(std::mem::replace(&mut batch, Self::Batch::default()));
                }
                current_index = index;
            }
            batch.add_system(system);
        }
        if !batch.systems().is_empty() {
            self.add_batch(batch);
        }
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
        for batch in self.batches_mut() {
            batch.run(parallelism, extra.clone(), resource_manager)?;
            resource_manager.unify_resources("IsSchedule::run after one")?;
        }
        self.batches_mut()
            .retain(|batch| !batch.systems().is_empty());

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

    fn get_schedule_names(&self) -> Vec<Vec<&str>> {
        self.batches()
            .iter()
            .map(|batch| {
                batch
                    .systems()
                    .iter()
                    .map(|sys| sys.name())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    }
}

/// Describes borrowing of system resources at runtime. For internal use,
/// mostly.
#[derive(Clone, Debug)]
pub struct Borrow {
    pub id: ResourceId,
    pub is_exclusive: bool,
}

impl Borrow {
    /// The resource id
    pub fn rez_id(&self) -> ResourceId {
        self.id.clone()
    }

    /// The type name of the resource
    pub fn name(&self) -> &str {
        self.id.name
    }

    /// Whether this borrow is mutable (`true`) or immutable (`false`).
    pub fn is_exclusive(&self) -> bool {
        self.is_exclusive
    }
}

fn borrows_conflict<'a>(borrows_a: &[Borrow], borrows_b: &[Borrow]) -> bool {
    for borrow_a in borrows_a {
        for borrow_b in borrows_b {
            if borrow_a.rez_id() == borrow_b.rez_id()
                && (borrow_a.is_exclusive() || borrow_b.is_exclusive())
            {
                return true;
            }
        }
    }
    false
}

/// Denotes a system's dependency on another.
#[derive(Clone, PartialEq)]
pub enum Dependency {
    After(String),
    Before(String),
}

mod solver {
    use anyhow;
    use cassowary::*;
    use std::ops::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    struct Sys(usize);
    cassowary::derive_syntax_for!(Sys);

    pub struct SolverSystem {
        pub name: String,
        pub dependencies: Vec<super::Dependency>,
        pub borrows: Vec<super::Borrow>,
        pub barrier: usize,
    }

    impl SolverSystem {
        fn must_run_after(&self, system_b: &SolverSystem) -> bool {
            self.dependencies
                .contains(&super::Dependency::After(system_b.name.clone()))
        }

        fn must_run_before(&self, system_b: &SolverSystem) -> bool {
            self.dependencies
                .contains(&super::Dependency::Before(system_b.name.clone()))
        }
    }

    pub fn solve_order(systems: &[SolverSystem]) -> anyhow::Result<Vec<f64>> {
        log::trace!("solving schedule for {} systems", systems.len());
        let mut systems = systems.iter().collect::<Vec<_>>();
        systems.sort_by(|a, b| a.barrier.cmp(&b.barrier));
        let max_barrier = systems.iter().fold(0, |b, sys| sys.barrier.max(b));
        let barriers = (0..max_barrier).map(|b| Sys(b)).collect::<Vec<_>>();
        log::trace!("  {} barriers", barriers.len());
        let mut solver: Solver<Sys> = cassowary::Solver::new();
        let mut constraints = vec![];
        for barrier_a in barriers.iter() {
            solver.add_constraint(barrier_a.is_ge(0.0)).unwrap();
            constraints.push(format!("barrier {} >= 0", barrier_a.0));
            log::trace!("  {}", constraints.last().unwrap());
            for barrier_b in barriers.iter() {
                if barrier_a.0 > barrier_b.0 {
                    solver
                        .add_constraint(barrier_a.is_ge(*barrier_b + 1.0))
                        .unwrap();
                    constraints.push(format!("barrier {} > barrier {}", barrier_a.0, barrier_b.0));
                    log::trace!("  {}", constraints.last().unwrap());
                }
            }
        }
        for (a, system_a) in systems.iter().enumerate() {
            let sys_a = Sys(a + max_barrier);

            if !barriers.is_empty() {
                let barrier = Sys(system_a.barrier);
                solver.add_constraint(sys_a.is_ge(barrier + 1.0)).unwrap();
                constraints.push(format!("{} > barrier {}", system_a.name, barrier.0));
                log::trace!("  {}", constraints.last().unwrap());
            }

            for (b, system_b) in systems.iter().enumerate() {
                if system_a.name == system_b.name {
                    continue;
                }

                let sys_b = Sys(b + max_barrier);
                let before_constraint = sys_b.is_ge(sys_a + 1.0);
                let before_msg = format!("{} > {}", system_b.name, system_a.name);
                let after_constraint = sys_a.is_ge(sys_b + 1.0);
                let after_msg = format!("{} > {}", system_a.name, system_b.name);

                if system_a.must_run_before(system_b) {
                    if !solver.has_constraint(&before_constraint) {
                        solver.add_constraint(before_constraint).map_err(|e| {
                            anyhow::anyhow!(
                                "can't make {:?} < {:?}: {:?}\nconstraints: {:#?}",
                                system_a.name,
                                system_b.name,
                                e,
                                constraints
                            )
                        })?;
                        log::trace!("  {}", before_msg);
                        constraints.push(before_msg);
                    }
                } else if system_a.must_run_after(system_b) {
                    if !solver.has_constraint(&after_constraint) {
                        solver.add_constraint(after_constraint).map_err(|e| {
                            anyhow::anyhow!(
                                "can't make {:?} > {:?}: {:?}\nconstraints: {:#?}",
                                system_a.name,
                                system_b.name,
                                e,
                                constraints
                            )
                        })?;
                        log::trace!("  {}", after_msg);
                        constraints.push(after_msg);
                    }
                }
            }
        }

        let out = systems
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let sys = Sys(i);
                solver.get_value(sys)
            })
            .collect::<Vec<_>>();
        Ok(out)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::system::*;

    #[test]
    fn schedule_with_dependencies() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut schedule = SyncSchedule::default();
        schedule.add_system(SyncSystem::new("one", |()| ok(), vec![]));
        schedule.add_system(SyncSystem::new(
            "two",
            |()| ok(),
            vec![Dependency::After("one".to_string())],
        ));
        schedule.add_system(SyncSystem::new(
            "three",
            |()| ok(),
            vec![Dependency::After("two".to_string())],
        ));
        schedule.add_system(SyncSystem::new(
            "three-again",
            |()| ok(),
            vec![
                Dependency::After("two".to_string()),
                Dependency::Before("four".to_string()),
            ],
        ));
        schedule.add_system(SyncSystem::new(
            "four",
            |()| ok(),
            vec![Dependency::After("three".to_string())],
        ));
        assert_eq!(
            vec![
                vec!["one"],
                vec!["two"],
                vec!["three", "three-again"],
                vec!["four"],
            ],
            schedule.get_schedule_names()
        );

        schedule.add_system(SyncSystem::new(
            "zero",
            |()| ok(),
            vec![Dependency::Before("one".to_string())],
        ));
        assert_eq!(
            vec![
                vec!["zero"],
                vec!["one"],
                vec!["two"],
                vec!["three", "three-again"],
                vec!["four"],
            ],
            schedule.get_schedule_names()
        );
    }

    #[test]
    fn schedule_with_barrier() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut schedule = SyncSchedule::default();
        schedule.add_system(SyncSystem::new("one", |()| ok(), vec![]));
        schedule.add_barrier();
        schedule.add_system(SyncSystem::new("two", |()| ok(), vec![]));
        schedule.add_system(SyncSystem::new("three", |()| ok(), vec![]));
        schedule.add_barrier();
        schedule.add_system(SyncSystem::new("four", |()| ok(), vec![]));

        assert_eq!(
            vec![vec!["one"], vec!["two", "three"], vec!["four"]],
            schedule.get_schedule_names()
        );
    }

    #[test]
    fn schedule_with_ephemeral() {
        let mut schedule = SyncSchedule::default();
        schedule.add_system(SyncSystem::new("one", |()| end(), vec![]));
        schedule.add_system(SyncSystem::new("two", |()| ok(), vec![]));
        schedule.add_system(SyncSystem::new("three", |()| ok(), vec![]));

        assert_eq!(
            vec![vec!["one", "two", "three"]],
            schedule.get_schedule_names()
        );

        let mut manager = ResourceManager::default();
        schedule.run((), &mut manager).unwrap();
        assert_eq!(vec![vec!["two", "three"]], schedule.get_schedule_names());
    }
}
