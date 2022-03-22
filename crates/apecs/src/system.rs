use std::{collections::HashMap, future::Future, pin::Pin};

use anyhow::Context;
use smol::future::FutureExt;

use crate::{mpsc, spsc, Request, Resource, ResourceId};

pub type AsyncSystemFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + Sync + 'static>>;

pub(crate) struct AsyncSystem {
    pub name: String,
    // The system logic as a future.
    pub future: AsyncSystemFuture,
    // Unbounded. Used to send resource requests from the system to the world.
    pub resource_request_rx: spsc::Receiver<Request>,
    // Bounded (1). Used to send resources from the world to the system.
    pub resources_to_system_tx: spsc::Sender<HashMap<ResourceId, Resource>>,
}

impl AsyncSystem {
    /// Tick the system and return true if the system should continue.
    pub fn tick_async_system(
        &mut self,
        cx: &mut std::task::Context,
        resources: &mut HashMap<ResourceId, Resource>,
    ) -> anyhow::Result<bool> {
        let mut sent_resources = None;

        // receive any system resource requests
        if let Some(Request { resource_ids }) = self.resource_request_rx.try_recv().ok() {
            let resources = crate::core::try_take_resources(resources, resource_ids.iter(), Some(&self.name))?;

            // send them to the system
            self
                .resources_to_system_tx
                .try_send(resources)
                .context(format!("cannot send resources to system '{}'", self.name))?;
            // save these for later to confirm that resources have been returned
            sent_resources = Some(resource_ids);
        }

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
        if let Some(sent_resources) = sent_resources.as_mut() {
            if cfg!(feature = "debug_async") {
                // ensure that all the sent resources have been given back
                sent_resources.retain(|k| !resources.contains_key(k));
            }
            // if we've received all the resources we sent
            if cfg!(feature = "debug_async") && !sent_resources.is_empty() {
                tracing::error!(
                        "system '{}' is holding onto world resources '{:?}' longer than one frame, downstream systems may fail",
                        self.name,
                        sent_resources.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
                    );
                if system_done {
                    anyhow::bail!(
                        "system '{}' finished but never returned resources",
                        self.name
                    );
                }
            }
        }

        Ok(!system_done)
    }
}

pub(crate) struct SyncSystem {
    pub name: String,
    pub reads: Vec<ResourceId>,
    pub writes: Vec<ResourceId>,
    pub function: SystemFunction,
}

pub type SystemFunction = Box<
    dyn FnMut(
            mpsc::Sender<(ResourceId, Resource)>,
            HashMap<ResourceId, Resource>,
        ) -> anyhow::Result<()>
        + 'static,
>;
