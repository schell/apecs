//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::{
    collections::HashMap,
    future::Future,
    sync::Arc,
    task::{RawWaker, Wake, Waker, Context as Ctx},
};

use anyhow::Context;

use smol::future::FutureExt;

use crate::{mpsc, AsyncSystem, CanFetch, IsResource, Resource, ResourceId, SyncSystem, Facade, spsc, storage::{CanReadStorage, CanWriteStorage}};

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
        let waker = self.waker.take().unwrap_or_else(|| {
            // create a dummy context for asyncs
            let raw_waker = RawWaker::from(Arc::new(DummyWaker));
            unsafe { Waker::from_raw(raw_waker) }
        });
        let mut cx = std::task::Context::from_waker(&waker);

        self.tick_with_context(Some(&mut cx))
    }

    /// Conduct a world tick but use an explicit context.
    /// If no context is given, async systems and futures will not be ticked.
    pub fn tick_with_context<'a>(&mut self, mut cx: Option<&mut Ctx>) -> anyhow::Result<()> {
        tracing::trace!("tick");

        // run through all the systems and poll them all
        for system in self.sync_systems.iter_mut() {
            tracing::trace!("running sync system '{}'", system.name);
            clear_returned_resources(
                Some(&system.name),
                &self.resources_from_system.1,
                &mut self.resources,
            )?;
            let resource_ids = system.reads.iter().chain(system.writes.iter());
            let resources =
                try_take_resources(&mut self.resources, resource_ids, Some(&system.name))?;
            (system.function)(self.resources_from_system.0.clone(), resources)?;
        }

        if let Some(cx) = cx.as_mut() {
            self.async_systems.retain_mut(|system| {
                tracing::trace!("running async system '{}'", system.name);
                clear_returned_resources(
                    Some(&system.name),
                    &self.resources_from_system.1,
                    &mut self.resources,
                )
                    .unwrap();
                system
                    .tick_async_system(cx, &mut self.resources)
                    .unwrap()
            });

            // run the non-system futures and remove the finished tasks
            if !self.tasks.is_empty() {
                let _ = self.executor.try_tick();
                for mut task in std::mem::take(&mut self.tasks) {
                    if let std::task::Poll::Pending = task.poll(cx) {
                        self.tasks.push(task);
                    }
                }
            }
        }

        Ok(())
    }

    pub fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        clear_returned_resources(None, &self.resources_from_system.1, &mut self.resources)?;

        let mut resource_ids = T::reads();
        resource_ids.extend(T::writes());
        let mut rezs = try_take_resources(&mut self.resources, resource_ids.iter(), None)?;
        T::construct(self.resources_from_system.0.clone(), &mut rezs)
    }

    pub fn new_waker() -> Waker {
        // create a dummy context for asyncs
        let raw_waker = RawWaker::from(Arc::new(DummyWaker));
        unsafe { Waker::from_raw(raw_waker) }
    }

    pub fn get_waker(&mut self) -> Waker {
        self.waker.take().unwrap_or_else(|| Self::new_waker())
    }

    /// Run all system and non-system futures until they have all finished or one
    /// system has erred, whichever comes first.
    pub fn run(&mut self) -> anyhow::Result<&mut Self> {
        let waker = self.get_waker();
        let mut cx = std::task::Context::from_waker(&waker);

        loop {
            self.tick_with_context(Some(&mut cx))?;
            if self.async_systems.is_empty()
                && self.sync_systems.is_empty()
                && self.tasks.is_empty()
            {
                break;
            }
        }

        self.waker = Some(waker);
        Ok(self)
    }
}
