use std::{collections::VecDeque, future::Future, pin::Pin, sync::atomic::AtomicU64};

use anyhow::Context;

use crate::{
    resource_manager::{LoanManager, ResourceManager},
    schedule::{Borrow, IsBatch, IsSchedule, IsSystem, UntypedSystemData},
    spsc,
    world::Facade,
    CanFetch, Request, Resource,
};

static SYSTEM_ITERATION: AtomicU64 = AtomicU64::new(0);

pub fn clear_iteration() {
    SYSTEM_ITERATION.store(0, std::sync::atomic::Ordering::SeqCst)
}

pub fn current_iteration() -> u64 {
    SYSTEM_ITERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// Increment the system iteration counter, returning the previous value.
pub fn increment_current_iteration() -> u64 {
    SYSTEM_ITERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// A future representing an async system.
pub type AsyncSystemFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + Sync + 'static>>;

/// A helper for creating async systems that take an initial state.
pub fn make_stateful_async_system<T, F, Fut>(
    state: T,
    f: F,
) -> impl FnOnce(Facade) -> AsyncSystemFuture
where
    T: Send + Sync + 'static,
    F: FnOnce(T, Facade) -> Fut,
    Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
{
    move |facade: Facade| Box::pin((f)(state, facade))
}

#[derive(Debug)]
pub struct AsyncSystem {
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

/// Everything is ok, the system should not be run again.
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

    fn prep(&self, loan_mngr: &mut LoanManager<'_>) -> anyhow::Result<UntypedSystemData> {
        (self.prepare)(loan_mngr)
    }

    fn run(&mut self, data: UntypedSystemData) -> anyhow::Result<ShouldContinue> {
        (self.function)(data)
    }
}

impl SyncSystem {
    pub fn new<T, F>(name: impl AsRef<str>, mut sys_fn: F) -> Self
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch + Send + Sync + 'static,
    {
        SyncSystem {
            name: name.as_ref().to_string(),
            borrows: T::borrows(),
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
}

#[derive(Debug, Default)]
pub struct SyncSchedule {
    batches: Vec<SyncBatch>,
    should_run_parallel: bool,
    current_barrier: usize,
}

impl IsSchedule for SyncSchedule {
    type System = SyncSystem;
    type Batch = SyncBatch;

    fn batches_mut(&mut self) -> &mut [Self::Batch] {
        &mut self.batches
    }

    fn batches(&self) -> &[Self::Batch] {
        &self.batches
    }

    fn add_batch(&mut self, batch: Self::Batch) {
        self.batches.push(batch);
    }

    fn set_should_parallelize(&mut self, should: bool) {
        self.should_run_parallel = should;
    }

    fn get_should_parallelize(&self) -> bool {
        self.should_run_parallel
    }

    fn current_barrier(&self) -> usize {
        self.current_barrier
    }

    fn add_barrier(&mut self) {
        self.current_barrier += 1;
    }
}

#[derive(Debug)]
pub struct AsyncSystemRequest<'a>(pub &'a AsyncSystem, pub Request);

/// In terms of system resource scheduling a request is a system.
impl<'a> IsSystem for AsyncSystemRequest<'a> {
    fn name(&self) -> &str {
        &self.0.name
    }

    fn borrows(&self) -> &[Borrow] {
        &self.1.borrows
    }

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
        _: bool,
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

        // tick the executor until the loaned resources have been returned
        loop {
            while extra.try_tick() {
                let _ = increment_current_iteration();
            }
            let resources_still_loaned = resource_manager.try_unify_resources("async batch")?;
            if resources_still_loaned {
                log::warn!("system in the batch is holding on to resources over an await point!");
            } else {
                log::trace!("all resources returned");
                break;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct AsyncSchedule<'a>(Vec<AsyncBatch<'a>>);

impl<'a> IsSchedule for AsyncSchedule<'a> {
    type System = AsyncSystemRequest<'a>;
    type Batch = AsyncBatch<'a>;

    fn batches_mut(&mut self) -> &mut [Self::Batch] {
        self.0.as_mut_slice()
    }

    fn batches(&self) -> &[Self::Batch] {
        self.0.as_slice()
    }

    fn add_batch(&mut self, batch: Self::Batch) {
        self.0.push(batch);
    }

    fn set_should_parallelize(&mut self, _: bool) {}

    fn get_should_parallelize(&self) -> bool {
        false
    }

    fn current_barrier(&self) -> usize {
        0
    }

    fn add_barrier(&mut self) {}
}
