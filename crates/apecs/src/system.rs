use std::{future::Future, pin::Pin};

use anyhow::Context;
use rustc_hash::FxHashMap;

use crate::{
    mpsc,
    schedule::{BatchData, Borrow, IsBatch, IsSchedule, IsSystem},
    spsc,
    world::{self, Facade},
    CanFetch, FetchReadyResource, Request, Resource, ResourceId,
};

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
    //pub future: AsyncSystemFuture,
    // Unbounded. Used to send resource requests from the system to the world.
    pub resource_request_rx: spsc::Receiver<Request>,
    // Bounded (1). Used to send resources from the world to the system.
    pub resources_to_system_tx: spsc::Sender<FxHashMap<ResourceId, FetchReadyResource>>,
}

/// Whether or not a system should continue execution.
pub enum ShouldContinue {
    Yes,
    No
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

pub type SystemFunction = Box<
    dyn FnMut(
            mpsc::Sender<(ResourceId, Resource)>,
            FxHashMap<ResourceId, FetchReadyResource>,
        ) -> anyhow::Result<ShouldContinue>
        + Send
        + Sync
        + 'static,
>;

pub struct SyncSystem {
    pub name: String,
    pub borrows: Vec<Borrow>,
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
    type Borrow = Borrow;

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn borrows(&self) -> &[Self::Borrow] {
        &self.borrows
    }

    fn run(
        &mut self,
        tx: mpsc::Sender<(ResourceId, Resource)>,
        resources: FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<ShouldContinue> {
        (self.function)(tx, resources)
    }
}

impl SyncSystem {
    pub fn new<T, F>(name: impl AsRef<str>, mut sys_fn: F) -> Self
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch,
    {
        SyncSystem {
            name: name.as_ref().to_string(),
            borrows: T::reads()
                .into_iter()
                .map(|id| Borrow {
                    id,
                    is_exclusive: false,
                })
                .chain(T::writes().into_iter().map(|id| Borrow {
                    id,
                    is_exclusive: true,
                }))
                .collect(),
            function: Box::new(move |tx, mut resources| {
                let data = T::construct(tx, &mut resources)?;
                sys_fn(data)
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
    type Borrow = Borrow;

    fn name(&self) -> &str {
        &self.0.name
    }

    fn borrows(&self) -> &[Self::Borrow] {
        &self.1.borrows
    }

    fn run(
        &mut self,
        _: mpsc::Sender<(ResourceId, Resource)>,
        resources: FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<ShouldContinue> {
        self.0
            .resources_to_system_tx
            .try_send(resources)
            .context(format!(
                "could not send resources to async system '{}'",
                self.0.name
            ))?;
        ok()
    }
}

#[derive(Debug, Default)]
pub struct AsyncBatch<'a>(Vec<AsyncSystemRequest<'a>>);

impl<'a> IsBatch for AsyncBatch<'a> {
    type System = AsyncSystemRequest<'a>;
    type ExtraRunData = &'a smol::Executor<'static>;


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
        mut data: BatchData<Self::ExtraRunData>,
        resources: &mut FxHashMap<ResourceId, Resource>,
        resources_from_system: &(
            mpsc::Sender<(ResourceId, Resource)>,
            mpsc::Receiver<(ResourceId, Resource)>,
        ),
    ) -> anyhow::Result<()> {
        let mut systems = self.take_systems();
        systems.retain(|system| {
            let data = data.resources.pop_front().unwrap();
            if system.0.resource_request_rx.is_closed() {
                return false;
            }
            // sometimes a system doesn't request resources every frame
            if !data.is_empty() {
                // send the resources off, if need be
                system
                    .0
                    .resources_to_system_tx
                    .try_send(data)
                    .with_context(|| {
                        format!(
                            "could not send resources to async system '{}'",
                            system.name()
                        )
                    })
                    .unwrap();
            }
            true
        });

        self.set_systems(systems);

        // tick the executor
        while data.extra.try_tick() {}

        world::clear_returned_resources(
            Some("batch"),
            &resources_from_system.1,
            resources,
            &mut data.borrowed_resources,
        )?;

        anyhow::ensure!(
            data.borrowed_resources.is_empty(),
            "shared batch resources are still in the wild"
        );

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
