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

pub type SystemFunction = Box<
    dyn FnMut(
            mpsc::Sender<(ResourceId, Resource)>,
            FxHashMap<ResourceId, FetchReadyResource>,
        ) -> anyhow::Result<()>
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
    ) -> anyhow::Result<()> {
        (self.function)(tx, resources)
    }
}

impl SyncSystem {
    pub fn new<T, F>(name: impl AsRef<str>, mut sys_fn: F) -> Self
    where
        F: FnMut(T) -> anyhow::Result<()> + Send + Sync + 'static,
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
pub struct SyncBatch(Vec<SyncSystem>);

impl IsBatch for SyncBatch {
    type System = SyncSystem;
    type ExtraRunData = ();

    fn systems(&self) -> &[Self::System] {
        &self.0
    }

    fn systems_mut(&mut self) -> &mut [Self::System] {
        &mut self.0
    }

    fn add_system(&mut self, system: Self::System) {
        self.0.push(system);
    }
}

#[derive(Debug, Default)]
pub struct SyncSchedule {
    batches: Vec<SyncBatch>,
    should_run_parallel: bool,
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
    ) -> anyhow::Result<()> {
        self.0
            .resources_to_system_tx
            .try_send(resources)
            .context(format!(
                "could not send resources to async system '{}'",
                self.0.name
            ))
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

    fn add_system(&mut self, system: Self::System) {
        self.0.push(system);
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
        // send the resources off, if need be
        self.systems()
            .iter()
            .zip(data.resources.into_iter())
            .try_for_each(|(system, data)| -> anyhow::Result<()> {
                // sometimes a system doesn't request resources every frame
                if !data.is_empty() {
                    system
                        .0
                        .resources_to_system_tx
                        .try_send(data)
                        .with_context(|| {
                            format!(
                                "could not send resources to async system '{}'",
                                system.name()
                            )
                        })?;
                }

                Ok(())
            })?;

        // tick the executor
        while data.extra.try_tick() {}

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
}
