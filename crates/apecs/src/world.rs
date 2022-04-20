//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::{future::Future, sync::Arc};

use anyhow::Context;
use rustc_hash::FxHashMap;

use crate::{
    mpsc,
    plugins::Plugin,
    schedule::{Borrow, IsBorrow, IsSchedule},
    spsc,
    system::{
        AsyncSchedule, AsyncSystem, AsyncSystemFuture, AsyncSystemRequest, ShouldContinue,
        SyncSchedule, SyncSystem,
    },
    CanFetch, FetchReadyResource, IsResource, Request, Resource, ResourceId,
};

pub struct Facade {
    // Unbounded. Sending a request from the system should not yield the async
    pub(crate) resource_request_tx: spsc::Sender<Request>,
    // Bounded(1). Awaiting in the system should yield the async
    pub(crate) resources_to_system_rx: spsc::Receiver<FxHashMap<ResourceId, FetchReadyResource>>,
    // Unbounded. Sending from the system should not yield the async
    pub(crate) resources_from_system_tx: mpsc::Sender<(ResourceId, Resource)>,
}

impl Facade {
    pub async fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        let reads = T::reads().into_iter().map(|id| Borrow {
            id,
            is_exclusive: false,
        });
        let writes = T::writes().into_iter().map(|id| Borrow {
            id,
            is_exclusive: true,
        });
        self.resource_request_tx
            .try_send(Request {
                borrows: reads.chain(writes).collect(),
            })
            .context("could not send request for resources")?;

        let mut resources = self
            .resources_to_system_rx
            .recv()
            .await
            .context("could not fetch resources")?;

        T::construct(self.resources_from_system_tx.clone(), &mut resources)
            .with_context(|| format!("could not construct {}", std::any::type_name::<T>()))
    }
}

pub struct World {
    // world resources
    resources: FxHashMap<ResourceId, Resource>,
    // resources loaned out to the owner of
    // this world
    loaned_resources: FxHashMap<ResourceId, Arc<Resource>>,
    // outer resource channel (unbounded)
    resources_from_system: (
        mpsc::Sender<(ResourceId, Resource)>,
        mpsc::Receiver<(ResourceId, Resource)>,
    ),
    sync_schedule: SyncSchedule,
    async_systems: Vec<AsyncSystem>,
    async_system_executor: smol::Executor<'static>,
    // executor for non-system futures
    async_task_executor: smol::Executor<'static>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            resources: Default::default(),
            loaned_resources: FxHashMap::default(),
            resources_from_system: mpsc::unbounded(),
            sync_schedule: SyncSchedule::default(),
            async_systems: vec![],
            async_system_executor: Default::default(),
            async_task_executor: Default::default(),
        }
    }
}

pub(crate) fn clear_returned_resources(
    system_name: Option<&str>,
    resources_from_system_rx: &mpsc::Receiver<(ResourceId, Resource)>,
    resources: &mut FxHashMap<ResourceId, Resource>,
    loaned_resources: &mut FxHashMap<ResourceId, Arc<Resource>>,
) -> anyhow::Result<()> {
    while let Ok((rez_id, resource)) = resources_from_system_rx.try_recv() {
        // put the resources back, there should be nothing stored there currently
        let prev = resources.insert(rez_id, resource);
        if cfg!(feature = "debug_async") && prev.is_some() {
            anyhow::bail!(
                "system '{}' sent back duplicate resources",
                system_name.unwrap_or("world")
            );
        }
    }

    // put the borrowed resources back
    if !loaned_resources.is_empty() {
        for (id, rez) in std::mem::take(loaned_resources).into_iter() {
            let rez = Arc::try_unwrap(rez)
                .map_err(|_| anyhow::anyhow!("could not retreive borrowed resource"))?;
            resources.insert(id, rez);
        }
    }

    Ok(())
}

pub(crate) fn try_take_resources<'a>(
    resources: &mut FxHashMap<ResourceId, Resource>,
    borrows: impl Iterator<Item = &'a (impl IsBorrow + 'a)>,
    system_name: Option<&str>,
) -> anyhow::Result<(
    FxHashMap<ResourceId, FetchReadyResource>,
    FxHashMap<ResourceId, Arc<Resource>>,
)> {
    // get only the requested resources
    let mut ready_resources: FxHashMap<ResourceId, FetchReadyResource> = FxHashMap::default();
    let mut stay_resources: FxHashMap<ResourceId, Arc<Resource>> = FxHashMap::default();
    for borrow in borrows {
        let rez: Resource = resources.remove(&borrow.rez_id()).with_context(|| {
            format!(
                r#"system '{}' requested missing resource "{}" encountered while building request for {:?}"#,
                system_name.unwrap_or("world"),
                borrow.rez_id().name,
                resources
                    .keys()
                    .map(|k| k.name)
                    .collect::<Vec<_>>(),
            )
        })?;

        let ready_rez = if borrow.is_exclusive() {
            FetchReadyResource::Owned(rez)
        } else {
            let stay_rez = Arc::new(rez);
            let ready_rez = FetchReadyResource::Ref(stay_rez.clone());
            let _ = stay_resources.insert(borrow.rez_id().clone(), stay_rez);
            ready_rez
        };
        let prev_inserted_rez = ready_resources.insert(borrow.rez_id(), ready_rez);
        assert!(
            prev_inserted_rez.is_none(),
            "cannot request multiple resources of the same type: '{:?}'",
            borrow.rez_id()
        );
    }

    Ok((ready_resources, stay_resources))
}

pub(crate) fn make_async_system_pack<F, Fut>(
    world: &World,
    name: String,
    make_system_future: F,
) -> (AsyncSystem, AsyncSystemFuture)
where
    F: FnOnce(Facade) -> Fut,
    Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
{
    let (resource_request_tx, resource_request_rx) = spsc::unbounded();
    let (resources_to_system_tx, resources_to_system_rx) =
        spsc::bounded::<FxHashMap<ResourceId, FetchReadyResource>>(1);
    let facade = Facade {
        resource_request_tx,
        resources_to_system_rx,
        resources_from_system_tx: world.resources_from_system.0.clone(),
    };
    let system = AsyncSystem {
        name: name.clone(),
        resource_request_rx,
        resources_to_system_tx,
    };
    let asys = Box::pin((make_system_future)(facade));
    (system, asys)
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

    pub fn set_resource<T: IsResource>(&mut self, resource: T) -> anyhow::Result<&mut Self> {
        let _prev = self
            .resources
            .insert(ResourceId::new::<T>(), Box::new(resource));
        //if let Some(prev) = prev {
        //    let t: Box<T> = prev.downcast()?;
        //    let t: T = t.into_inner();
        //}
        Ok(self)
    }

    pub fn with_plugin(&mut self, plugin: impl Into<Plugin>) -> &mut Self {
        let plugin = plugin.into();

        for lazy_rez in plugin.resources.into_iter() {
            if !self.resources.contains_key(lazy_rez.id()) {
                let (id, resource) = lazy_rez.into();
                let _ = self.resources.insert(id, resource);
            }
        }

        for system in plugin.sync_systems.into_iter() {
            if !self.sync_schedule.contains_system(&system.0.name) {
                self.sync_schedule
                    .add_system_with_dependecies(system.0, system.1.into_iter());
            }
        }

        'outer: for asystem in plugin.async_systems.into_iter() {
            for asys in self.async_systems.iter() {
                if asys.name == asystem.0 {
                    continue 'outer;
                }
            }

            let (sys, fut) = make_async_system_pack(&self, asystem.0, asystem.1);
            self.add_async_system_pack(sys, fut);
        }

        self
    }

    pub fn with_system<T, F>(&mut self, name: impl AsRef<str>, sys_fn: F) -> &mut Self
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch,
    {
        self.with_system_with_dependencies(name, &[], sys_fn)
    }

    pub fn with_system_with_dependencies<T, F>(
        &mut self,
        name: impl AsRef<str>,
        deps: &[&str],
        sys_fn: F,
    ) -> &mut Self
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch,
    {
        let system = SyncSystem::new(name, sys_fn);

        self.sync_schedule
            .add_system_with_dependecies(system, deps.iter());
        self
    }

    pub fn with_sync_systems_run_in_parallel(&mut self, parallelize: bool) -> &mut Self {
        self.sync_schedule.set_should_parallelize(parallelize);
        self
    }

    pub(crate) fn add_async_system_pack(&mut self, system: AsyncSystem, fut: AsyncSystemFuture) {
        let name = system.name.clone();
        self.async_systems.push(system);

        let future = async move {
            match fut.await {
                Ok(()) => {}
                Err(err) => {
                    let msg = err
                        .chain()
                        .map(|err| format!("{}", err))
                        .collect::<Vec<_>>()
                        .join("\n  ");
                    tracing::error!("async system '{}' erred: {}", name, msg);
                }
            }
        };

        let task = self.async_system_executor.spawn(future);
        task.detach();
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
        let (system, fut) =
            make_async_system_pack(&self, name.as_ref().to_string(), make_system_future);
        self.add_async_system_pack(system, fut);

        self
    }

    pub fn with_async(
        &mut self,
        future: impl Future<Output = ()> + Send + Sync + 'static,
    ) -> &mut Self {
        self.spawn(future);
        self
    }

    /// Spawn a non-system asynchronous task.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + Sync + 'static) {
        let task = self.async_task_executor.spawn(future);
        task.detach();
    }

    /// Conduct a world tick but use an explicit context.
    /// If no context is given, async systems and futures will not be ticked.
    pub fn tick(&mut self) -> anyhow::Result<()> {
        self.tick_async()?;
        self.tick_sync()
    }

    /// Just tick the synchronous systems.
    pub fn tick_sync(&mut self) -> anyhow::Result<()> {
        clear_returned_resources(
            None,
            &self.resources_from_system.1,
            &mut self.resources,
            &mut self.loaned_resources,
        )?;

        // run the scheduled sync systems
        self.sync_schedule
            .run((), &mut self.resources, &self.resources_from_system)
    }

    /// Just tick the async futures, including sending resources to async
    /// systems.
    pub fn tick_async(&mut self) -> anyhow::Result<()> {
        clear_returned_resources(
            None,
            &self.resources_from_system.1,
            &mut self.resources,
            &mut self.loaned_resources,
        )
        .unwrap();

        // trim the systems that may request resources by checking their
        // resource request/return channels
        self.async_systems.retain(|system| {
            // if the channels are closed they have been dropped by the
            // async system and we should no longer poll them for requests
            if system.resource_request_rx.is_closed() {
                debug_assert!(system.resources_to_system_tx.is_closed());
                false
            } else {
                true
            }
        });

        // fetch all the requests for system resources and fold them into a schedule
        let mut schedule =
            self.async_systems
                .iter()
                .fold(AsyncSchedule::default(), |mut schedule, system| {
                    let async_request = match system.resource_request_rx.try_recv() {
                        Ok(request) => AsyncSystemRequest(&system, request),
                        Err(err) => match err {
                            // return an async system that requests nothing
                            smol::channel::TryRecvError::Empty => {
                                AsyncSystemRequest(&system, Request::default())
                            }
                            smol::channel::TryRecvError::Closed => unreachable!(),
                        },
                    };
                    schedule.add_system(async_request);
                    schedule
                });

        if !schedule.is_empty() {
            schedule.run(
                &self.async_system_executor,
                &mut self.resources,
                &self.resources_from_system,
            )?;
        }

        // lastly tick all our non-system tasks
        while self.async_task_executor.try_tick() {}

        Ok(())
    }

    pub fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        clear_returned_resources(
            None,
            &self.resources_from_system.1,
            &mut self.resources,
            &mut self.loaned_resources,
        )?;

        let reads = T::reads().into_iter().map(|id| Borrow {
            id,
            is_exclusive: false,
        });
        let writes = T::writes().into_iter().map(|id| Borrow {
            id,
            is_exclusive: true,
        });
        let borrows = reads.chain(writes).collect::<Vec<_>>();
        let (mut rezs, staying_refs) =
            try_take_resources(&mut self.resources, borrows.iter(), None)?;
        self.loaned_resources.extend(staying_refs);
        T::construct(self.resources_from_system.0.clone(), &mut rezs)
    }

    /// Run all system and non-system futures until they have all finished or one
    /// system has erred, whichever comes first.
    ///
    /// This will return even if there are general non-system futures in progress.
    pub fn run(&mut self) -> anyhow::Result<&mut Self> {
        loop {
            self.tick()?;
            if self.async_systems.is_empty() && self.sync_schedule.is_empty() {
                break;
            }
        }

        Ok(self)
    }

    pub fn get_schedule_description(&self) -> String {
        format!("{:#?}", self.sync_schedule)
    }
}

#[cfg(test)]
mod test {
    use crate as apecs;
    use apecs::{anyhow, entities::*, join::*, spsc, storage::*, system::*, world::*, Read, Write};

    #[test]
    fn can_closure_system() -> anyhow::Result<()> {
        #[derive(CanFetch)]
        struct StatefulSystemData {
            positions: Read<VecStorage<(f32, f32)>>,
        }

        fn mk_stateful_system(
            tx: spsc::Sender<(f32, f32)>,
        ) -> impl FnMut(StatefulSystemData) -> anyhow::Result<ShouldContinue> {
            let mut highest_pos: (f32, f32) = (0.0, f32::NEG_INFINITY);

            move |data: StatefulSystemData| {
                for pos in data.positions.iter() {
                    if pos.1 > highest_pos.1 {
                        highest_pos = *pos.value();
                    }
                }

                tx.try_send(highest_pos)?;

                ok()
            }
        }

        let mut positions: VecStorage<(f32, f32)> = VecStorage::default();
        positions.insert(0, (20.0, 30.0));
        positions.insert(1, (0.0, 0.0));
        positions.insert(2, (100.0, 100.0));

        let (tx, rx) = spsc::bounded(1);

        let mut world = World::default();
        world
            .with_resource(positions)?
            .with_system("stateful", mk_stateful_system(tx));

        world.tick()?;

        let highest = rx.try_recv()?;
        assert_eq!(highest, (100.0, 100.0));

        Ok(())
    }

    #[test]
    fn async_systems_run_and_return_resources() -> anyhow::Result<()> {
        async fn create(tx: spsc::Sender<()>, mut facade: Facade) -> anyhow::Result<()> {
            println!("create running");
            tx.try_send(()).unwrap();
            let (mut entities, mut names, mut numbers): (
                Write<Entities>,
                Write<VecStorage<String>>,
                Write<VecStorage<u32>>,
            ) = facade.fetch().await?;
            for n in 0..100 {
                let e = entities.create();
                let _ = names.insert(e.id(), format!("entity_{}", n));
                let _ = numbers.insert(e.id(), n);
            }

            Ok(())
        }

        fn maintain_map(
            mut data: (
                Read<VecStorage<String>>,
                Read<VecStorage<u32>>,
                Write<FxHashMap<String, u32>>,
            ),
        ) -> anyhow::Result<ShouldContinue> {
            for (name, number) in (&data.0, &data.1).join() {
                if !data.2.contains_key(name.value()) {
                    let _ = data.2.insert(name.to_string(), *number.value());
                }
            }

            ok()
        }

        let subscriber = tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");

        let (tx, rx) = spsc::bounded(1);
        let mut world = World::default();
        world
            .with_default_resource::<Entities>()?
            .with_default_resource::<VecStorage<String>>()?
            .with_default_resource::<VecStorage<u32>>()?
            .with_default_resource::<FxHashMap<String, u32>>()?
            .with_async_system("create", |facade| async move { create(tx, facade).await })
            .with_system("maintain", maintain_map);

        // create system runs - sending on the channel and making the fetch request
        world.tick()?;
        rx.try_recv().unwrap();

        // world sends resources to the create system which makes
        // entities+components, then the maintain system updates the book
        world.tick()?;

        let book = world.fetch::<Read<FxHashMap<String, u32>>>()?;
        for n in 0..100 {
            assert_eq!(book.get(&format!("entity_{}", n)), Some(&n));
        }

        Ok(())
    }
}
