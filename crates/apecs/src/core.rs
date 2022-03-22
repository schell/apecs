//! Core types and processes
//!
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    future::Future,
    ops::{Deref, DerefMut},
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

mod fetch;
pub use fetch::*;

use crate::system::*;

pub trait IsResource: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> IsResource for T {}

pub type Resource = Box<dyn Any + Send + Sync + 'static>;

#[derive(Clone, Debug, Eq)]
pub struct ResourceId {
    pub(crate) type_id: TypeId,
    pub(crate) name: String,
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
pub struct Fetched<T: IsResource> {
    // should be unbounded, `send` should never fail
    resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
    inner: Option<Box<T>>,
}

impl<'a, T: IsResource> Deref for Fetched<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource> DerefMut for Fetched<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl<'a, T: IsResource> Drop for Fetched<T> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            self.resource_return_tx
                .try_send((ResourceId::new::<T>(), inner as Resource))
                .unwrap();
        }
    }
}

pub struct Write<T: IsResource> {
    fetched: Fetched<T>,
}

impl<'a, T: IsResource> Deref for Write<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.fetched.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource> DerefMut for Write<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.fetched.inner.as_mut().unwrap()
    }
}

pub struct Read<T: IsResource> {
    fetched: Fetched<T>,
}

impl<'a, T: IsResource> Deref for Read<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.fetched.inner.as_ref().unwrap()
    }
}

pub(crate) struct Request {
    pub resource_ids: Vec<ResourceId>,
}

pub struct Facade {
    // Unbounded. Sending from the system should not yield the async
    resource_request_tx: spsc::Sender<Request>,
    // Bounded(1). Awaiting in the system should yield the async
    resources_to_system_rx: spsc::Receiver<HashMap<ResourceId, Resource>>,
    // Unbounded. Sending from the system should not yield the async
    resources_from_system_tx: mpsc::Sender<(ResourceId, Resource)>,
}

impl Facade {
    pub async fn fetch<'a, T: CanFetch>(&'a mut self) -> anyhow::Result<T> {
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
        T::construct(self.resources_from_system_tx.clone(), &mut resources)
            .context("could not construct system data")
    }
}

pub trait CanFetch: Sized {
    fn reads() -> Vec<ResourceId>;
    fn writes() -> Vec<ResourceId>;
    fn construct<'a>(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut HashMap<ResourceId, Resource>,
    ) -> anyhow::Result<Self>;
}

impl<'a, T: IsResource> CanFetch for Write<T> {
    fn writes() -> Vec<ResourceId> {
        vec![ResourceId::new::<T>()]
    }

    fn reads() -> Vec<ResourceId> {
        vec![]
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
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

impl<'a, T: IsResource> CanFetch for Read<T> {
    fn writes() -> Vec<ResourceId> {
        vec![]
    }

    fn reads() -> Vec<ResourceId> {
        vec![ResourceId::new::<T>()]
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
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

struct DummyWaker;

impl Wake for DummyWaker {
    fn wake(self: std::sync::Arc<Self>) {}
}

pub struct World {
    waker: Option<Waker>,
    // world resources
    resources: HashMap<ResourceId, Resource>,
    // outer resource channel (unbounded)
    resources_from_system: (
        mpsc::Sender<(ResourceId, Resource)>,
        mpsc::Receiver<(ResourceId, Resource)>,
    ),
    sync_systems: Vec<SyncSystem>,
    async_systems: Vec<AsyncSystem>,
    // executor for non-system futures
    executor: smol::Executor<'static>,
    // handles of all non-system futures
    tasks: Vec<smol::Task<()>>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            waker: None,
            resources: Default::default(),
            resources_from_system: mpsc::unbounded(),
            sync_systems: vec![],
            async_systems: vec![],
            executor: Default::default(),
            tasks: vec![],
        }
    }
}

fn clear_returned_resources(
    system_name: Option<&str>,
    resources_from_system_rx: &mpsc::Receiver<(ResourceId, Resource)>,
    resources: &mut HashMap<ResourceId, Resource>,
) -> anyhow::Result<()> {
    while let Some((rez_id, resource)) = resources_from_system_rx.try_recv().ok() {
        // put the resources back, there should be nothing stored there currently
        let prev = resources.insert(rez_id, resource);
        if cfg!(feature = "debug_async") && prev.is_some() {
            anyhow::bail!(
                "system '{}' sent back duplicate resources",
                system_name.unwrap_or_else(|| "world")
            );
        }
    }

    Ok(())
}

pub(crate) fn try_take_resources<'a>(
    resources: &mut HashMap<ResourceId, Resource>,
    resource_ids: impl Iterator<Item = &'a ResourceId>,
    system_name: Option<&str>,
) -> anyhow::Result<HashMap<ResourceId, Resource>> {
    // get only the requested resources
    let mut system_resources: HashMap<ResourceId, Resource> = HashMap::default();
    for rez_id in resource_ids {
        let rez: Resource = resources.remove(rez_id).with_context(|| {
                        format!(
                            r#"system '{}' requested missing resource "{}" encountered while building request for {:?}"#,
                            system_name.unwrap_or_else(|| "world"),
                            rez_id.name,
                            resources
                                .keys()
                                .map(|k| k.name.as_str())
                                .collect::<Vec<_>>(),
                        )
                    })?;
        assert!(
            system_resources.insert(rez_id.clone(), rez).is_none(),
            "cannot request multiple resources of the same type: '{:?}'",
            rez_id
        );
    }

    Ok(system_resources)
}

impl World {
    pub fn with_default_resource<T: Default + IsResource>(&mut self) -> anyhow::Result<&mut Self> {
        let resource: T = T::default();
        self.with_resource(resource)
    }

    pub fn with_resource<T: IsResource>(&mut self, resource: T) -> anyhow::Result<&mut Self> {
        if self
            .resources
            .insert(ResourceId::new::<T>(), Box::new(resource))
            .is_some()
        {
            anyhow::bail!("resource {} already exists", std::any::type_name::<T>());
        }

        Ok(self)
    }

    pub fn with_system<T, F>(&mut self, name: impl AsRef<str>, mut sys_fn: F) -> &mut Self
    where
        F: FnMut(T) -> anyhow::Result<()> + 'static,
        T: CanFetch,
    {
        let system = SyncSystem {
            name: name.as_ref().to_string(),
            reads: T::reads(),
            writes: T::writes(),
            function: Box::new(move |tx, mut resources| {
                let data = T::construct(tx, &mut resources)?;
                sys_fn(data)
            }),
        };

        self.sync_systems.push(system);
        self
    }

    pub fn with_async_system<F, Fut>(
        &mut self,
        name: impl AsRef<str>,
        make_system_future: F,
    ) -> &mut Self
    where
        F: FnOnce(Facade) -> Fut,
        Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
    {
        let (resource_request_tx, resource_request_rx) = spsc::unbounded();
        let (resources_to_system_tx, resources_to_system_rx) = spsc::bounded(1);

        let facade = Facade {
            resource_request_tx,
            resources_to_system_rx,
            resources_from_system_tx: self.resources_from_system.0.clone(),
        };

        let system = AsyncSystem {
            name: name.as_ref().to_string(),
            future: Box::pin((make_system_future)(facade)),
            resource_request_rx,
            resources_to_system_tx,
        };

        self.async_systems.push(system);
        self
    }

    pub fn with_async(
        &mut self,
        future: impl Future<Output = ()> + Send + Sync + 'static,
    ) -> &mut Self {
        self.spawn(future);
        self
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + Send + Sync + 'static) {
        let task = self.executor.spawn(future);
        task.detach();
    }

    pub fn tick(&mut self) -> anyhow::Result<()> {
        tracing::trace!("tick");

        let waker = self.waker.take().unwrap_or_else(|| {
            // create a dummy context for asyncs
            let raw_waker = RawWaker::from(Arc::new(DummyWaker));
            unsafe { Waker::from_raw(raw_waker) }
        });
        let mut cx = std::task::Context::from_waker(&waker);

        // run through all the systems and poll them all
        for system in self.sync_systems.iter_mut() {
            tracing::trace!("running sync system '{}'", system.name);
            clear_returned_resources(
                Some(&system.name),
                &self.resources_from_system.1,
                &mut self.resources,
            )?;
            let resource_ids = system.reads.iter().chain(system.writes.iter());
            let resources = try_take_resources(&mut self.resources, resource_ids, Some(&system.name))?;
            (system.function)(self.resources_from_system.0.clone(), resources)?;
        }

        self.async_systems.retain_mut(|system| {
            tracing::trace!("running async system '{}'", system.name);
            clear_returned_resources(
                Some(&system.name),
                &self.resources_from_system.1,
                &mut self.resources,
            ).unwrap();
            system.tick_async_system(&mut cx, &mut self.resources).unwrap()
        });

        // run the non-system futures and remove the finished tasks
        if !self.tasks.is_empty() {
            let _ = self.executor.try_tick();
            for mut task in std::mem::take(&mut self.tasks) {
                if let std::task::Poll::Pending = task.poll(&mut cx) {
                    self.tasks.push(task);
                }
            }
        }

        self.waker = Some(waker);

        Ok(())
    }

    pub fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        clear_returned_resources(None, &self.resources_from_system.1, &mut self.resources)?;

        let mut resource_ids = T::reads();
        resource_ids.extend(T::writes());
        let mut rezs = try_take_resources(&mut self.resources, resource_ids.iter(), None)?;
        T::construct(self.resources_from_system.0.clone(), &mut rezs)
    }

    /// Run all system and non-system futures until they have all finished or one
    /// system has erred, whichever comes first.
    pub fn run(&mut self) -> anyhow::Result<&mut Self> {
        loop {
            self.tick()?;
            if self.async_systems.is_empty()
                && self.sync_systems.is_empty()
                && self.tasks.is_empty()
            {
                return Ok(self);
            }
        }
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
    fn smol_executor_sanity() {
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

        let (a, _b) = msgs.split_at(4);
        assert_eq!(
            a,
            &[
                "tick".to_string(),
                "A 0".to_string(),
                "tick".to_string(),
                "B 0".to_string()
            ]
        );
    }
}
