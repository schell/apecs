use std::{future::Future, pin::Pin, sync::Arc};

use anyhow::Context;
use rustc_hash::FxHashMap;
use smol::future::FutureExt;

use crate::{
    mpsc,
    schedule::{IsBatch, IsBorrow, IsSchedule, IsSystem},
    spsc, FetchReadyResource, Request, Resource, ResourceId,
};

pub type AsyncSystemFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + Sync + 'static>>;

// TODO: Add async systems as general futures and simply schedule requests for
// resources in the world tick, then tick the smol executor until the resources
// come back.
pub(crate) struct AsyncSystem {
    pub name: String,
    // The system logic as a future.
    pub future: AsyncSystemFuture,
    // Unbounded. Used to send resource requests from the system to the world.
    pub resource_request_rx: spsc::Receiver<Request>,
    // Bounded (1). Used to send resources from the world to the system.
    pub resources_to_system_tx: spsc::Sender<FxHashMap<ResourceId, FetchReadyResource>>,
}

impl AsyncSystem {
    /// Tick the system and return true if the system should continue.
    pub fn tick_async_system(
        &mut self,
        cx: &mut std::task::Context,
        resources: &mut FxHashMap<ResourceId, Resource>,
    ) -> anyhow::Result<bool> {
        let mut sent_resources = None;

        // receive any system resource requests
        let borrowed_resources = if let Ok(mut request) = self.resource_request_rx.try_recv() {
            // get the outgoing resources (owned and refs) and a clone of the
            // refs
            let (resources, staying_refs) = crate::world::try_take_resources(
                resources,
                request.borrows.iter(),
                Some(&self.name),
            )?;

            // send them to the system
            self.resources_to_system_tx
                .try_send(resources)
                .context(format!("cannot send resources to system '{}'", self.name))?;
            // save the request for later to confirm that the owned resources have been returned
            request
                .borrows
                .retain(|b| !staying_refs.contains_key(&b.id));
            sent_resources = Some(request);
            staying_refs
        } else {
            FxHashMap::default()
        };

        // run the system, bailing if it errs
        let system_done = match self.future.poll(cx) {
            std::task::Poll::Ready(res) => {
                match res {
                    Ok(()) => {
                        tracing::trace!("system '{}' has ended gracefully", self.name);
                    }
                    Err(err) => anyhow::bail!("system '{}' has erred: {}", self.name, err),
                }
                true
            }
            std::task::Poll::Pending => false,
        };

        // try to get the resources back, if possible
        if let Some(request) = sent_resources.as_mut() {
            if cfg!(feature = "debug_async") {
                // ensure that all the sent resources have been given back
                request.borrows.retain(|k| !resources.contains_key(&k.id));
            }
            // if we've received all the resources we sent
            if cfg!(feature = "debug_async") && !request.borrows.is_empty() {
                tracing::error!(
                        "system '{}' is holding on to world resources '{:?}' longer than one frame, downstream systems may fail",
                        self.name,
                        request.borrows.iter().map(|k| k.id.name).collect::<Vec<_>>(),
                    );
                if system_done {
                    anyhow::bail!(
                        "system '{}' finished but never returned resources",
                        self.name
                    );
                }
            }
        }

        // put the borrowed resources back
        for (id, rez) in borrowed_resources.into_iter() {
            let rez = Arc::try_unwrap(rez)
                .map_err(|_| anyhow::anyhow!("could not retreive borrowed resource"))?;
            resources.insert(id, rez);
        }

        Ok(!system_done)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Borrow {
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

pub type SystemFunction = Box<
    dyn FnMut(
            mpsc::Sender<(ResourceId, Resource)>,
            FxHashMap<ResourceId, FetchReadyResource>,
        ) -> anyhow::Result<()>
        + Send
        + Sync
        + 'static,
>;

pub(crate) struct SyncSystem {
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

    fn conflicts_with(&self, other: &impl IsSystem) -> bool {
        for borrow_here in self.borrows.iter() {
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
        tx: mpsc::Sender<(ResourceId, Resource)>,
        resources: FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<()> {
        (self.function)(tx, resources)
    }
}

#[derive(Debug, Default)]
pub struct SyncBatch(Vec<SyncSystem>);

impl IsBatch for SyncBatch {
    type System = SyncSystem;

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
