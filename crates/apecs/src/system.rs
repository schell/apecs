use std::{collections::VecDeque, future::Future, pin::Pin, sync::atomic::AtomicU64};

use anyhow::Context;
use rayon::prelude::*;

use super::{
    resource_manager::{LoanManager, ResourceManager},
    schedule::{Borrow, IsBatch, IsSchedule, IsSystem, UntypedSystemData, Dependency},
    chan::spsc,
    CanFetch, Request, Resource,
};

static SYSTEM_ITERATION: AtomicU64 = AtomicU64::new(0);

#[inline]
/// Get the current system iteration timestamp.
pub fn current_iteration() -> u64 {
    SYSTEM_ITERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Increment the system iteration counter, returning the previous value.
#[inline]
pub(crate) fn increment_current_iteration() -> u64 {
    SYSTEM_ITERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A future representing an async system.
pub type AsyncSystemFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + Sync + 'static>>;

#[derive(Debug)]
pub(crate) struct AsyncSystem {
    pub name: String,
    // The system logic as a future.
    // pub future: AsyncSystemFuture,
    // Unbounded. Used to send resource requests from the system to the world.
    pub resource_request_rx: spsc::Receiver<Request>,
    // Bounded (1). Used to send resources from the world to the system.
    pub resources_to_system_tx: spsc::Sender<Resource>,
}

/// Whether or not a system should continue execution.
pub enum ShouldContinue {
    Yes,
    No,
}

/// Everything is ok, the system should continue.
pub fn ok() -> anyhow::Result<ShouldContinue> {
    Ok(ShouldContinue::Yes)
}

/// Everything is ok, but the system should not be run again.
pub fn end() -> anyhow::Result<ShouldContinue> {
    Ok(ShouldContinue::No)
}

/// An error occured.
pub fn err(err: anyhow::Error) -> anyhow::Result<ShouldContinue> {
    Err(err)
}

pub type SystemFunction =
    Box<dyn FnMut(UntypedSystemData) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static>;

pub struct SyncSystem {
    pub name: String,
    pub borrows: Vec<Borrow>,
    pub dependencies: Vec<Dependency>,
    pub barrier: usize,
    pub prepare: fn(&mut LoanManager<'_>) -> anyhow::Result<UntypedSystemData>,
    pub function: SystemFunction,
}

impl std::fmt::Debug for SyncSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncSystem")
            .field("name", &self.name)
            .field("borrows", &self.borrows)
            .field("function", &"FnMut(_)")
            .finish()
    }
}

impl IsSystem for SyncSystem {
    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn borrows(&self) -> &[Borrow] {
        &self.borrows
    }

    fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }

    fn barrier(&self) -> usize {
        self.barrier
    }

    fn set_barrier(&mut self, barrier: usize) {
        self.barrier = barrier;
    }

    fn prep(&self, loan_mngr: &mut LoanManager<'_>) -> anyhow::Result<UntypedSystemData> {
        (self.prepare)(loan_mngr)
    }

    fn run(&mut self, data: UntypedSystemData) -> anyhow::Result<ShouldContinue> {
        (self.function)(data)
    }
}

impl SyncSystem {
    pub fn new<T, F>(name: impl AsRef<str>, mut sys_fn: F, dependencies: Vec<Dependency>) -> Self
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch + Send + Sync + 'static,
    {
        SyncSystem {
            name: name.as_ref().to_string(),
            borrows: T::borrows(),
            dependencies,
            barrier: 0,
            prepare: |loan_mngr: &mut LoanManager| {
                let box_t: Box<T> = Box::new(T::construct(loan_mngr)?);
                let b: UntypedSystemData = box_t;
                Ok(b)
            },
            function: Box::new(move |b: UntypedSystemData| {
                let box_t: Box<T> = b.downcast().ok().context("cannot downcast")?;
                let t: T = *box_t;
                sys_fn(t)
            }),
        }
    }
}

#[derive(Debug, Default)]
pub struct SyncBatch(Vec<SyncSystem>, usize);

impl IsBatch for SyncBatch {
    type System = SyncSystem;
    type ExtraRunData = ();

    fn systems(&self) -> &[Self::System] {
        &self.0
    }

    fn systems_mut(&mut self) -> &mut [Self::System] {
        &mut self.0
    }

    fn trim_systems(&mut self, should_remove: rustc_hash::FxHashSet<&str>) {
        self.0.retain(|sys| !should_remove.contains(sys.name()))
    }

    fn add_system(&mut self, system: Self::System) {
        self.0.push(system);
    }

    fn get_barrier(&self) -> usize {
        self.1
    }

    fn set_barrier(&mut self, barrier: usize) {
        self.1 = barrier;
    }

    fn take_systems(&mut self) -> Vec<Self::System> {
        std::mem::replace(&mut self.0, vec![])
    }

    fn set_systems(&mut self, systems: Vec<Self::System>) {
        self.0 = systems;
    }

    fn run(
        &mut self,
        parallelism: u32,
        _: Self::ExtraRunData,
        resource_manager: &mut ResourceManager,
    ) -> anyhow::Result<()> {
        let mut loan_mngr = LoanManager(resource_manager);
        let systems = self.take_systems();
        let mut data = vec![];
        for sys in systems.iter() {
            data.push(sys.prep(&mut loan_mngr)?);
        }
        let (remaining_systems, errs): (Vec<_>, Vec<_>) = if parallelism > 1 {
            let available_threads = rayon::current_num_threads();
            if parallelism > available_threads as u32 {
                log::warn!(
                    "the rayon threadpool does not contain enough threads! requested {}, have {}",
                    parallelism,
                    available_threads
                );
            }
            (systems, data)
                .into_par_iter()
                .filter_map(|(mut system, data)| {
                    log::trace!("running par system '{}'", system.name());
                    let _ = increment_current_iteration();
                    match system.run(data) {
                        Ok(ShouldContinue::Yes) => Some(rayon::iter::Either::Left(system)),
                        Ok(ShouldContinue::No) => None,
                        Err(err) => Some(rayon::iter::Either::Right(err)),
                    }
                })
                .partition_map(|e| e)
        } else {
            let mut remaining_systems = vec![];
            let mut errs = vec![];
            systems
                .into_iter()
                .zip(data.into_iter())
                .for_each(|(mut system, data)| {
                    log::trace!("running system '{}'", system.name());
                    let _ = increment_current_iteration();
                    match system.run(data) {
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

        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct SyncSchedule {
    batches: Vec<SyncBatch>,
    num_threads: u32,
    current_barrier: usize,
}

impl IsSchedule for SyncSchedule {
    type System = SyncSystem;
    type Batch = SyncBatch;

    fn batches_mut(&mut self) -> &mut Vec<Self::Batch> {
        &mut self.batches
    }

    fn batches(&self) -> &[Self::Batch] {
        &self.batches
    }

    fn add_batch(&mut self, batch: Self::Batch) {
        self.batches.push(batch);
    }

    fn set_parallelism(&mut self, threads: u32) {
        self.num_threads = threads;
    }

    fn get_parallelism(&self) -> u32 {
        self.num_threads
    }

    fn current_barrier(&self) -> usize {
        self.current_barrier
    }

    fn add_barrier(&mut self) {
        self.current_barrier += 1;
    }
}

#[derive(Debug)]
pub(crate) struct AsyncSystemRequest<'a>(pub &'a AsyncSystem, pub Request);

/// In terms of system resource scheduling a request is a system.
impl<'a> IsSystem for AsyncSystemRequest<'a> {
    fn name(&self) -> &str {
        &self.0.name
    }

    fn borrows(&self) -> &[Borrow] {
        &self.1.borrows
    }

    fn dependencies(&self) -> &[Dependency] {
        &[]
    }

    fn barrier(&self) -> usize {
        0
    }

    fn set_barrier(&mut self, _:usize) {}

    fn prep(&self, loan_mngr: &mut LoanManager<'_>) -> anyhow::Result<UntypedSystemData> {
        (self.1.construct)(loan_mngr)
    }

    fn run(&mut self, data: UntypedSystemData) -> anyhow::Result<ShouldContinue> {
        self.0
            .resources_to_system_tx
            .try_send(data)
            .context(format!(
                "could not send resources to async system '{}'",
                self.name()
            ))?;
        ok()
    }
}

#[derive(Debug, Default)]
pub struct AsyncBatch<'a>(Vec<AsyncSystemRequest<'a>>);

impl<'a> IsBatch for AsyncBatch<'a> {
    type System = AsyncSystemRequest<'a>;
    type ExtraRunData = &'a async_executor::Executor<'static>;

    fn systems(&self) -> &[Self::System] {
        self.0.as_slice()
    }

    fn systems_mut(&mut self) -> &mut [Self::System] {
        self.0.as_mut_slice()
    }

    fn trim_systems(&mut self, should_remove: rustc_hash::FxHashSet<&str>) {
        self.0.retain(|s| !should_remove.contains(s.name()));
    }

    fn add_system(&mut self, system: Self::System) {
        self.0.push(system);
    }

    fn get_barrier(&self) -> usize {
        0
    }

    fn set_barrier(&mut self, _: usize) {}

    fn take_systems(&mut self) -> Vec<Self::System> {
        std::mem::replace(&mut self.0, vec![])
    }

    fn set_systems(&mut self, systems: Vec<Self::System>) {
        self.0 = systems;
    }

    fn run(
        &mut self,
        parallelism: u32,
        extra: &'a async_executor::Executor<'static>,
        resource_manager: &mut ResourceManager,
    ) -> anyhow::Result<()> {
        let mut loan_mngr = LoanManager(resource_manager);
        let mut systems = self.take_systems();
        let mut data = VecDeque::new();
        for sys in systems.iter() {
            data.push_back(sys.prep(&mut loan_mngr)?);
        }
        drop(loan_mngr);

        systems.retain_mut(|system| {
            let data: UntypedSystemData = data.pop_front().unwrap();
            if system.0.resource_request_rx.is_closed() {
                return false;
            }
            // send the resources off, if need be
            log::trace!("sending resources to async '{}'", system.name());
            let _ = system.run(data).unwrap();
            true
        });

        self.set_systems(systems);

        fn tick(executor: &async_executor::Executor<'static>) {
            while executor.try_tick() {
                let _ = increment_current_iteration();
            }
        }
        // tick the executor until the loaned resources have been returned
        loop {
            if parallelism > 1 {
                (0..parallelism as u32).into_par_iter().for_each(|_| tick(extra));
            } else {
                tick(extra);
            }
            let resources_still_loaned = resource_manager.try_unify_resources("async batch")?;
            if resources_still_loaned {
                log::warn!(
                    "an async system is holding onto resources over an await point! systems:{:#?}",
                    self.systems()
                        .iter()
                        .map(|sys| sys.name())
                        .collect::<Vec<_>>()
                );
            } else {
                log::trace!("all resources returned");
                break;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct AsyncSchedule<'a> {
    batches: Vec<AsyncBatch<'a>>,
    num_threads: u32
}

impl<'a> IsSchedule for AsyncSchedule<'a> {
    type System = AsyncSystemRequest<'a>;
    type Batch = AsyncBatch<'a>;

    fn batches_mut(&mut self) -> &mut Vec<Self::Batch> {
        &mut self.batches
    }

    fn batches(&self) -> &[Self::Batch] {
        self.batches.as_slice()
    }

    fn add_batch(&mut self, batch: Self::Batch) {
        self.batches.push(batch);
    }

    fn set_parallelism(&mut self, threads: u32) {
        self.num_threads = threads;
    }

    fn get_parallelism(&self) -> u32 {
        self.num_threads
    }

    fn current_barrier(&self) -> usize {
        0
    }

    fn add_barrier(&mut self) {}
}
