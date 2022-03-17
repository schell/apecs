//! Core types and processes
//!
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    future::Future,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::Arc,
    task::{RawWaker, Wake, Waker},
};

use anyhow::Context;
use smol::future::FutureExt;

pub use apecs_derive::CanFetch;

pub mod mpsc {
    pub use smol::channel::*;
}

pub mod spsc {
    pub use smol::channel::*;
}

pub trait IsResource: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> IsResource for T {}

pub type Resource = Box<dyn Any + Send + Sync + 'static>;

#[derive(Clone, Debug, Eq)]
pub struct ResourceId {
    type_id: TypeId,
    name: String,
}

impl std::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name.as_str())
    }
}

impl std::hash::Hash for ResourceId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.type_id.hash(state);
    }
}

impl PartialEq for ResourceId {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id
    }
}

impl ResourceId {
    pub fn new<T: IsResource>() -> Self {
        ResourceId {
            type_id: TypeId::of::<T>(),
            name: std::any::type_name::<T>().to_string(),
        }
    }
}

/// Wrapper for one fetched resource.
///
/// When dropped, the wrapped resource will be sent back to the world.
pub struct Fetched<'a, T: IsResource> {
    // should be unbounded, `send` should never fail
    resource_return_tx: &'a mpsc::Sender<(ResourceId, Resource)>,
    inner: Option<Box<T>>,
}

impl<'a, T: IsResource> Deref for Fetched<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource> DerefMut for Fetched<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl<'a, T: IsResource> Drop for Fetched<'a, T> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            self.resource_return_tx
                .try_send((ResourceId::new::<T>(), inner as Resource))
                .unwrap();
        }
    }
}

pub struct Write<'a, T: IsResource> {
    fetched: Fetched<'a, T>,
}

impl<'a, T: IsResource> Deref for Write<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.fetched.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource> DerefMut for Write<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.fetched.inner.as_mut().unwrap()
    }
}

pub struct Read<'a, T: IsResource> {
    fetched: Fetched<'a, T>,
}

impl<'a, T: IsResource> Deref for Read<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.fetched.inner.as_ref().unwrap()
    }
}

struct Request {
    resource_ids: Vec<ResourceId>,
}

pub struct Facade {
    // Unbounded. Sending from the system should not yield the async
    resource_request_tx: spsc::Sender<Request>,
    // Bounded(1). Awaiting in the system should yield the async
    resources_to_system_rx: spsc::Receiver<HashMap<ResourceId, Resource>>,
    // Unbounded. Sending from the system should not yield the async
    resources_from_system_tx: spsc::Sender<(ResourceId, Resource)>,
}

impl Facade {
    pub async fn fetch<'a, T: CanFetch<'a>>(&'a mut self) -> anyhow::Result<T> {
        let reads = T::reads();
        let writes = T::writes();
        let mut resource_ids = reads;
        resource_ids.extend(writes);
        self.resource_request_tx
            .try_send(Request { resource_ids })
            .context("could not send request for resources")?;

        let mut resources = self
            .resources_to_system_rx
            .recv()
            .await
            .context("could not fetch resources")?;
        T::construct(&self.resources_from_system_tx, &mut resources)
            .context("could not construct system data")
    }
}

pub trait CanFetch<'a>: Sized {
    fn reads() -> Vec<ResourceId>;
    fn writes() -> Vec<ResourceId>;
    fn construct(
        resource_return_tx: &'a spsc::Sender<(ResourceId, Resource)>,
        fields: &mut HashMap<ResourceId, Resource>,
    ) -> anyhow::Result<Self>;
    //fn deconstruct(self) -> HashMap<ResourceId, Resource>;
}

impl<'a, T: IsResource> CanFetch<'a> for Write<'a, T> {
    fn writes() -> Vec<ResourceId> {
        vec![ResourceId::new::<T>()]
    }

    fn reads() -> Vec<ResourceId> {
        vec![]
    }

    fn construct(
        resource_return_tx: &'a spsc::Sender<(ResourceId, Resource)>,
        fields: &mut HashMap<ResourceId, Resource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: Resource = fields.remove(&id).context(format!(
            "could not find '{}' in resources",
            std::any::type_name::<T>(),
        ))?;
        let inner: Option<Box<T>> = Some(t.downcast::<T>().map_err(|_| {
            anyhow::anyhow!(
                "could not cast resource as '{}'",
                std::any::type_name::<T>(),
            )
        })?);
        let fetched = Fetched {
            resource_return_tx,
            inner,
        };
        Ok(Write { fetched })
    }

    //fn deconstruct(self) -> HashMap<ResourceId, Resource> {
    //    HashMap::from([(ResourceId::new::<T>(), self.inner as Resource)])
    //}
}

impl<'a, T: IsResource> CanFetch<'a> for Read<'a, T> {
    fn writes() -> Vec<ResourceId> {
        vec![]
    }

    fn reads() -> Vec<ResourceId> {
        vec![ResourceId::new::<T>()]
    }

    fn construct(
        resource_return_tx: &'a spsc::Sender<(ResourceId, Resource)>,
        fields: &mut HashMap<ResourceId, Resource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: Resource = fields.remove(&id).context(format!(
            "could not find '{}' in resources",
            std::any::type_name::<T>(),
        ))?;
        let inner: Option<Box<T>> = Some(t.downcast().map_err(|_| {
            anyhow::anyhow!(
                "could not cast resource as '{}'",
                std::any::type_name::<T>(),
            )
        })?);
        let fetched = Fetched {
            resource_return_tx,
            inner,
        };
        Ok(Read { fetched })
    }

    //fn deconstruct(self) -> HashMap<ResourceId, Resource> {
    //    HashMap::from([(ResourceId::new::<T>(), self.inner as Resource)])
    //}
}

struct AsyncSystem {
    name: String,
    // The system logic as a future.
    future: AsyncSystemFuture,
    // Unbounded. Used to send resource requests from the system to the world.
    resource_request_rx: spsc::Receiver<Request>,
    // Bounded (1). Used to send resources from the world to the system.
    resources_to_system_tx: spsc::Sender<HashMap<ResourceId, Resource>>,
    // Unbounded. Used to send resources from the system back to the world.
    resources_from_system_rx: spsc::Receiver<(ResourceId, Resource)>,
}

struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: std::sync::Arc<Self>) {}
}

pub struct World {
    // world resources
    resources: HashMap<ResourceId, Resource>,
    // all systems
    systems: Vec<AsyncSystem>,
    // executor for non-system futures
    executor: smol::Executor<'static>,
    // handles of all non-system futures
    tasks: Vec<smol::Task<()>>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            resources: Default::default(),
            systems: vec![],
            executor: Default::default(),
            tasks: vec![],
        }
    }
}

impl World {
    pub fn add_default_resource<T: Default + IsResource>(&mut self) -> anyhow::Result<()> {
        let resource: T = T::default();
        self.add_resource(resource)
    }

    pub fn add_resource<T: IsResource>(&mut self, resource: T) -> anyhow::Result<()> {
        if self
            .resources
            .insert(ResourceId::new::<T>(), Box::new(resource))
            .is_some()
        {
            anyhow::bail!("resource {} already exists", std::any::type_name::<T>());
        }

        Ok(())
    }

    pub fn spawn_system(
        &mut self,
        name: impl AsRef<str>,
        make_system_future: impl MakeAsyncSystemFuture,
    ) {
        let (resource_request_tx, resource_request_rx) = spsc::unbounded();
        let (resources_to_system_tx, resources_to_system_rx) = spsc::bounded(1);
        let (resources_from_system_tx, resources_from_system_rx) = spsc::unbounded();

        let facade = Facade {
            resource_request_tx,
            resources_to_system_rx,
            resources_from_system_tx,
        };

        let system = AsyncSystem {
            name: name.as_ref().to_string(),
            future: make_system_future.make_system(facade),
            resource_request_rx,
            resources_to_system_tx,
            resources_from_system_rx,
        };

        self.systems.push(system);
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + Send + Sync + 'static) {
        let task = self.executor.spawn(future);
        task.detach();
    }

    pub fn tick(&mut self) -> anyhow::Result<()> {
        tracing::trace!("tick");
        // create a dummy context for asyncs
        let raw_waker = RawWaker::from(Arc::new(DummyWaker));
        let waker = unsafe { Waker::from_raw(raw_waker) };
        let mut cx = std::task::Context::from_waker(&waker);

        // run through all the systems and poll them all
        for mut system in std::mem::take(&mut self.systems).into_iter() {
            tracing::trace!("running system '{}'", system.name);
            let mut sent_resources = None;

            // receive any system resource requests
            if let Some(Request { resource_ids }) = system.resource_request_rx.try_recv().ok() {
                if cfg!(debug_assertions) {
                    for rez_id in &resource_ids {
                        assert!(
                            self.resources.contains_key(rez_id),
                            r#"system '{}' requested missing resource "{}", have: {:?} encountered while building request for {:?} - please make sure previous systems have returned their resources"#,
                            system.name,
                            rez_id.name,
                            self.resources
                                .keys()
                                .map(|k| k.name.as_str())
                                .collect::<Vec<_>>(),
                            resource_ids
                                .iter()
                                .map(|k| k.name.as_str())
                                .collect::<Vec<_>>()
                        )
                    }
                }
                //resource_ids.dedup();
                // get only the requested resources
                let mut resources: HashMap<ResourceId, Resource> = HashMap::default();
                for rez_id in &resource_ids {
                    let rez: Resource = self.resources.remove(rez_id).with_context(|| {
                        format!(
                            r#"system '{}' requested missing resource "{}", have: {:?} encountered while building request for {:?}"#,
                            system.name,
                            rez_id.name,
                            self.resources
                                .keys()
                                .map(|k| k.name.as_str())
                                .collect::<Vec<_>>(),
                            resource_ids.iter().map(|k| k.name.as_str()).collect::<Vec<_>>()
                        )
                    })?;
                    assert!(
                        resources.insert(rez_id.clone(), rez).is_none(),
                        "cannot request multiple resources of the same type: '{:?}'",
                        rez_id
                    );
                }
                // send them to the system
                system
                    .resources_to_system_tx
                    .try_send(resources)
                    .context(format!("cannot send resources to system '{}'", system.name))?;
                // save these for later to confirm that resources have been returned
                sent_resources = Some(resource_ids);
            }

            // run the system, bailing if it errs
            let system_done = match system.future.poll(&mut cx) {
                std::task::Poll::Ready(res) => {
                    match res {
                        Ok(()) => {
                            tracing::trace!("system '{}' has ended gracefully", system.name);
                        }
                        Err(err) => anyhow::bail!("system '{}' has erred: {}", system.name, err),
                    }
                    true
                }
                std::task::Poll::Pending => false,
            };

            // try to get the resources back, if possible
            if let Some(sent_resources) = sent_resources.as_mut() {
                while let Some((rez_id, resource)) = system.resources_from_system_rx.try_recv().ok() {
                    if cfg!(debug_assertions) {
                        sent_resources.retain(|k| k != &rez_id);
                    }
                    // put the resources back, there should be nothing stored there currently
                    let prev = self.resources.insert(rez_id, resource);
                    if cfg!(debug_assertions) && prev.is_some() {
                        anyhow::bail!("system '{}' sent back duplicate resources", system.name);
                    }
                }

                // if we've received all the resources we sent
                if cfg!(debug_assertions) && !sent_resources.is_empty() {
                    tracing::error!(
                        "system '{}' is holding onto world resources '{:?}' longer than one frame, downstream systems may fail",
                        system.name,
                        sent_resources.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
                    );
                    if system_done {
                        anyhow::bail!(
                            "system '{}' finished but never returned resources",
                            system.name
                        );
                    }
                }
            }

            if !system_done {
                // push the system back on to run for another frame
                self.systems.push(system);
            }
        }

        // run the non-system futures and remove the finished tasks
        let _ = self.executor.try_tick();
        for mut task in std::mem::take(&mut self.tasks) {
            if let std::task::Poll::Pending = task.poll(&mut cx) {
                self.tasks.push(task);
            }
        }

        Ok(())
    }

    /// Run all system and non-system futures until they have all finished or one
    /// system has erred, whichever comes first.
    pub fn run(&mut self) -> anyhow::Result<()> {
        loop {
            self.tick()?;
            if self.systems.is_empty() && self.tasks.is_empty() {
                return Ok(());
            }
        }
    }
}

pub type AsyncSystemFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + Sync + 'static>>;

pub trait MakeAsyncSystemFuture {
    fn make_system(self, data: Facade) -> AsyncSystemFuture;
}

impl<F, Fut> MakeAsyncSystemFuture for F
where
    F: FnOnce(Facade) -> Fut,
    Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
{
    fn make_system(self, data: Facade) -> AsyncSystemFuture {
        Box::pin((self)(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbounded_channel_doesnt_yield_on_send_and_await() {
        let (tx, rx) = mpsc::unbounded::<u32>();

        let executor = smol::Executor::new();
        let _t = executor.spawn(async move {
            for i in 0..5u32 {
                tx.send(i).await.unwrap();
            }
        });

        assert!(executor.try_tick());

        let mut msgs = vec![];
        while let Some(msg) = rx.try_recv().ok() {
            msgs.push(msg);
        }

        assert_eq!(msgs, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn it_works() {
        let (tx, rx) = mpsc::bounded::<String>(1);

        let executor = smol::Executor::new();
        let tx_t = tx.clone();
        let _t = executor.spawn(async move {
            let mut n = 0;
            loop {
                tx_t.send(format!("A {}", n)).await.unwrap();
                n += 1;
            }
        });

        let tx_s = tx.clone();
        let _s = executor.spawn(async move {
            let mut n = 0;
            loop {
                tx_s.send(format!("B {}", n)).await.unwrap();
                n += 1;
            }
        });

        let mut msgs = vec![];
        for _ in 0..10 {
            msgs.push("tick".to_string());
            let _ = executor.try_tick();
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
        }

        panic!("{:?}", msgs);
    }
}
