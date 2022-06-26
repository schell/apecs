//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::future::Future;
use std::{iter::Map, ops::Deref};

use anyhow::Context;
use hibitset::{BitIter, BitSet, BitSetLike};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::resource_manager::ResourceManager;
use crate::WriteExpect;
use crate::{
    mpsc,
    plugins::{entity_upkeep, Plugin},
    schedule::{Borrow, IsSchedule},
    spsc,
    storage::{CanWriteStorage, StoredComponent, WorldStorage},
    system::{
        AsyncSchedule, AsyncSystem, AsyncSystemFuture, AsyncSystemRequest, ShouldContinue,
        SyncSchedule, SyncSystem,
    },
    CanFetch, FetchReadyResource, IsResource, Request, Resource, ResourceId, ResourceRequirement,
    Write,
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

pub struct Lazy(mpsc::Sender<LazyOp>);

impl Lazy {
    pub fn exec_mut(
        &self,
        op: impl FnOnce(&mut World) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        self.0
            .try_send(LazyOp(Box::new(op)))
            .context("could not send lazy op")?;
        Ok(())
    }
}

pub struct LazyOp(Box<dyn FnOnce(&mut World) -> anyhow::Result<()> + Send + Sync + 'static>);

#[derive(Clone)]
pub struct Entity {
    id: usize,
    gen: usize,
    op_sender: mpsc::Sender<LazyOp>,
}

impl Deref for Entity {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.id
    }
}

impl Entity {
    pub fn id(&self) -> usize {
        self.id
    }

    /// Lazily add a component.
    ///
    /// This entity will have the associated component after the next tick.
    pub fn lazy_with<C: StoredComponent>(self, component: C) -> anyhow::Result<Self>
    where
        C::StorageType: WorldStorage,
    {
        let id = self.id;
        let op = Box::new(move |world: &mut World| -> anyhow::Result<()> {
            if !world.has_resource::<C::StorageType>() {
                world.with_default_storage::<C>()?;
            }
            let mut storage: Write<C::StorageType> = world.fetch()?;
            let _ = storage.insert(id, component);
            Ok(())
        });
        self.op_sender
            .try_send(LazyOp(op))
            .context("could not send entity op")?;
        Ok(self)
    }

    /// Lazily add a component and return a future that completes after the
    /// component has been added.
    pub fn lazy_add<C: StoredComponent>(
        &self,
        component: C,
    ) -> anyhow::Result<impl Future<Output = ()>>
    where
        C::StorageType: WorldStorage,
    {
        let (tx, rx) = spsc::bounded(1);
        let id = self.id;
        let op = LazyOp(Box::new(move |world: &mut World| -> anyhow::Result<()> {
            if !world.has_resource::<C::StorageType>() {
                world.with_default_storage::<C>()?;
            }
            let mut storage: Write<C::StorageType> = world.fetch()?;
            let _ = storage.insert(id, component);
            tx.try_send(())
                .context("could not send component add confirmation")?;
            Ok(())
        }));
        self.op_sender
            .try_send(op)
            .context("could not send entity op")?;
        Ok(async move {
            rx.recv().await.unwrap();
        })
    }
}

pub struct EntityBuilder<'a> {
    entity: Entity,
    world: &'a mut World,
}

impl<'a> EntityBuilder<'a> {
    /// Adds a component immediately.
    pub fn with<C: StoredComponent>(self, component: C) -> anyhow::Result<Self> {
        self.world
            .resource_manager
            .unify_resources("EntityBuilder::with")?;

        if !self.world.has_resource::<C::StorageType>() {
            self.world.with_default_storage::<C>()?;
        }
        let mut storage: Write<C::StorageType> = self.world.fetch()?;
        let _ = storage.insert(self.entity.id(), component);
        Ok(self)
    }

    /// Build and return the entity
    pub fn build(self) -> Entity {
        self.entity
    }
}

pub struct Entities {
    pub next_k: usize,
    pub alive_set: BitSet,
    pub dead: Vec<Entity>,
    pub recycle: Vec<Entity>,
    pub lazy_op_sender: mpsc::Sender<LazyOp>,
}

impl Default for Entities {
    fn default() -> Self {
        Self {
            next_k: Default::default(),
            alive_set: Default::default(),
            dead: Default::default(),
            recycle: Default::default(),
            lazy_op_sender: mpsc::unbounded().0,
        }
    }
}

impl Entities {
    pub fn new(lazy_op_sender: mpsc::Sender<LazyOp>) -> Self {
        Self {
            next_k: 0,
            alive_set: BitSet::new(),
            dead: Default::default(),
            recycle: Default::default(),
            lazy_op_sender,
        }
    }

    pub fn create(&mut self) -> Entity {
        let entity = if self.recycle.is_empty() {
            let id = self.next_k;
            self.next_k += 1;
            Entity {
                id,
                gen: 0,
                op_sender: self.lazy_op_sender.clone(),
            }
        } else {
            self.recycle.pop().unwrap()
        };

        self.alive_set.add(entity.id.try_into().unwrap());
        entity
    }

    pub fn destroy(&mut self, entity: Entity) {
        self.alive_set.remove(entity.id.try_into().unwrap());
        self.dead.push(entity);
    }

    pub fn iter(&self) -> Map<BitIter<&BitSet>, fn(u32) -> usize> {
        (&self.alive_set).iter().map(|id| id as usize)
    }

    pub fn recycle_dead(&mut self) {
        for mut dead in std::mem::take(&mut self.dead).into_iter() {
            dead.gen += 1;
            self.recycle.push(dead);
        }
    }
}

pub struct World {
    pub resource_manager: ResourceManager,
    pub sync_schedule: SyncSchedule,
    pub async_systems: Vec<AsyncSystem>,
    pub async_system_executor: async_executor::Executor<'static>,
    // executor for non-system futures
    pub async_task_executor: async_executor::Executor<'static>,
    pub lazy_ops: (mpsc::Sender<LazyOp>, mpsc::Receiver<LazyOp>),
}

impl Default for World {
    fn default() -> Self {
        let lazy_ops = mpsc::unbounded();
        let lazy = Lazy(lazy_ops.0.clone());
        let entities = Entities::new(lazy_ops.0.clone());
        let mut world = Self {
            resource_manager: ResourceManager::new(),
            sync_schedule: SyncSchedule::default(),
            async_systems: vec![],
            async_system_executor: Default::default(),
            async_task_executor: Default::default(),
            lazy_ops,
        };
        world
            .with_resource(lazy)
            .unwrap()
            .with_resource(entities)
            .unwrap();
        world
    }
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
        resources_from_system_tx: world.resource_manager.exclusive_resource_return_sender(),
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
        if self.resource_manager.add(resource).is_some() {
            anyhow::bail!("resource {} already exists", std::any::type_name::<T>());
        }

        Ok(self)
    }

    pub fn set_resource<T: IsResource>(&mut self, resource: T) -> anyhow::Result<&mut Self> {
        let _prev = self.resource_manager.add(resource);
        //if let Some(prev) = prev {
        //    let t: Box<T> = prev.downcast()?;
        //    let t: T = t.into_inner();
        //}
        Ok(self)
    }

    pub fn with_plugin(&mut self, plugin: impl Into<Plugin>) -> anyhow::Result<&mut Self> {
        let plugin = plugin.into();

        let mut missing_required_resources = FxHashSet::default();
        for req_rez in plugin.resources.into_iter() {
            let id = req_rez.id();
            if !self.resource_manager.has_resource(id) {
                log::trace!("missing resource {}...", id.name);
                match req_rez {
                    ResourceRequirement::ExpectedExisting(id) => {
                        log::warn!("...and it was expected!");
                        missing_required_resources.insert(id);
                    }
                    ResourceRequirement::LazyDefault(lazy_rez) => {
                        log::trace!("...so we're creating it with default");
                        let (id, resource) = lazy_rez.into();
                        missing_required_resources.remove(&id);
                        let _ = self.resource_manager.insert(id, resource);
                    }
                }
            }
        }

        anyhow::ensure!(
            missing_required_resources.is_empty(),
            "missing required resources:\n{:#?}",
            missing_required_resources
        );

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

        Ok(self)
    }

    pub fn with_data<T: CanFetch>(&mut self) -> anyhow::Result<&mut Self> {
        self.with_plugin(T::plugin())
    }

    pub fn with_storage<T: StoredComponent>(
        &mut self,
        store: T::StorageType,
    ) -> anyhow::Result<&mut Self> {
        self.with_resource(store)?
            .with_plugin(entity_upkeep::plugin::<T::StorageType>())
    }

    pub fn with_default_storage<T: StoredComponent>(&mut self) -> anyhow::Result<&mut Self> {
        let store = <T::StorageType>::default();
        self.with_resource(store)?
            .with_plugin(entity_upkeep::plugin::<T::StorageType>())
    }

    pub fn entity(&mut self) -> anyhow::Result<EntityBuilder<'_>> {
        let mut entities = self.fetch::<WriteExpect<Entities>>()?;
        let entity = entities.create();
        Ok(EntityBuilder {
            world: self,
            entity,
        })
    }

    pub fn with_system<T, F>(
        &mut self,
        name: impl AsRef<str>,
        sys_fn: F,
    ) -> anyhow::Result<&mut Self>
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
    ) -> anyhow::Result<&mut Self>
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch,
    {
        let system = SyncSystem::new(name, sys_fn);

        self.with_plugin(T::plugin())?;
        self.sync_schedule
            .add_system_with_dependecies(system, deps.iter());
        Ok(self)
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
                    log::error!("async system '{}' erred: {}", name, msg);
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

    /// Returns whether a resources of the given type exists in the world.
    pub fn has_resource<T: IsResource>(&self) -> bool {
        self.resource_manager.has_resource(&ResourceId::new::<T>())
    }

    /// Spawn a non-system asynchronous task.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + Sync + 'static) {
        let task = self.async_task_executor.spawn(future);
        task.detach();
    }

    /// Conduct a world tick.
    ///
    /// Calls `World::tick_async`, then `World::tick_sync`, then `World::tick_lazy`.
    pub fn tick(&mut self) -> anyhow::Result<()> {
        self.tick_async()?;
        self.tick_sync()?;
        self.tick_lazy()?;
        log::trace!(" ");
        Ok(())
    }

    /// Just tick the synchronous systems.
    pub fn tick_sync(&mut self) -> anyhow::Result<()> {
        log::trace!("tick sync");
        // run the scheduled sync systems
        self.sync_schedule.run((), &mut self.resource_manager)?;
        Ok(())
    }

    /// Just tick the async futures, including sending resources to async
    /// systems.
    pub fn tick_async(&mut self) -> anyhow::Result<()> {
        log::trace!("tick async");

        // trim the systems that may request resources by checking their
        // resource request/return channels
        self.async_systems.retain(|system| {
            // if the channels are closed they have been dropped by the
            // async system and we should no longer poll them for requests
            if system.resource_request_rx.is_closed() {
                debug_assert!(system.resources_to_system_tx.is_closed());
                log::trace!(
                    "removing {} from the system resource requester pool, it has dropped its \
                     request channel",
                    system.name
                );
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
                            async_channel::TryRecvError::Empty => {
                                AsyncSystemRequest(&system, Request::default())
                            }
                            async_channel::TryRecvError::Closed => unreachable!(),
                        },
                    };
                    schedule.add_system(async_request);
                    schedule
                });

        if !schedule.is_empty() {
            log::trace!(
                "async system execution:\n{}",
                schedule.get_execution_order().join("\n")
            );
            schedule.run(&self.async_system_executor, &mut self.resource_manager)?;
        }

        // lastly tick all our non-system tasks
        if !self.async_task_executor.is_empty() {
            let mut ticks = 0;
            while self.async_task_executor.try_tick() {
                ticks += 1;
            }
            log::trace!("ticked {} futures", ticks);
        }

        Ok(())
    }

    /// Applies lazy world updates.
    pub fn tick_lazy(&mut self) -> anyhow::Result<()> {
        log::trace!("tick lazy");

        while let Ok(LazyOp(op)) = self.lazy_ops.1.try_recv() {
            self.resource_manager.unify_resources("World::tick_lazy")?;
            (op)(self)?;
        }

        Ok(())
    }

    /// Attempt to get a reference to one resource.
    pub fn resource_get<T: IsResource>(&self) -> anyhow::Result<&T> {
        let id = ResourceId::new::<T>();
        self.resource_manager.get(&id)
    }

    /// Attempt to get a mutable reference to one resource.
    pub fn resource_get_mut<T: IsResource>(&mut self) -> anyhow::Result<&mut T> {
        let id = ResourceId::new::<T>();
        self.resource_manager.get_mut(&id)
    }

    pub fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        self.resource_manager.unify_resources("World::fetch")?;

        let reads = T::reads().into_iter().map(|id| Borrow {
            id,
            is_exclusive: false,
        });
        let writes = T::writes().into_iter().map(|id| Borrow {
            id,
            is_exclusive: true,
        });
        let borrows = reads.chain(writes).collect::<Vec<_>>();

        let mut resources = FxHashMap::default();
        self.resource_manager
            .try_loan_resources("World::fetch", &mut resources, borrows.iter())?;
        let t = T::construct(
            self.resource_manager.exclusive_resource_return_sender(),
            &mut resources,
        )?;
        anyhow::ensure!(
            resources.is_empty(),
            "{}::construct did not use all requested resources",
            std::any::type_name::<T>()
        );
        Ok(t)
    }

    /// Run all system and non-system futures until they have all finished or one
    /// system has erred, whichever comes first.
    pub fn run(&mut self) -> anyhow::Result<&mut Self> {
        loop {
            self.tick()?;
            if self.async_systems.is_empty()
                && self.sync_schedule.is_empty()
                && self.async_task_executor.is_empty()
            {
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
    use apecs::{anyhow, join::*, spsc, storage::*, system::*, world::*, Read, Write, WriteExpect};

    #[test]
    fn can_closure_system() -> anyhow::Result<()> {
        #[derive(CanFetch)]
        struct StatefulSystemData {
            positions: Read<VecStorage<(f32, f32)>>,
        }

        fn mk_stateful_system(
            tx: spsc::Sender<(f32, f32)>,
        ) -> impl FnMut(StatefulSystemData) -> anyhow::Result<ShouldContinue> {
            println!("making stateful system");
            let mut highest_pos: (f32, f32) = (0.0, f32::NEG_INFINITY);

            move |data: StatefulSystemData| {
                println!("running stateful system: highest_pos:{:?}", highest_pos);
                for pos in data.positions.iter() {
                    if pos.1 > highest_pos.1 {
                        highest_pos = *pos.value();
                        println!("set new highest_pos: {:?}", highest_pos);
                    }
                }

                println!("sending highest_pos: {:?}", highest_pos);
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
            .with_system("stateful", mk_stateful_system(tx))?;

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
                WriteExpect<Entities>,
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

        let (tx, rx) = spsc::bounded(1);
        let mut world = World::default();
        world
            .with_async_system("create", |facade| async move { create(tx, facade).await })
            .with_system("maintain", maintain_map)?;

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

    #[test]
    fn can_run_async_systems_that_both_borrow_reads() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        struct DataOne(u32, mpsc::Sender<u32>);

        async fn asys(mut facade: Facade) -> anyhow::Result<()> {
            log::info!("running asys");
            {
                let data = facade.fetch::<WriteExpect<DataOne>>().await?;
                log::info!("asys fetched data");

                // hold the fetched data over another await point
                use async_timer::oneshot::Oneshot;
                async_timer::oneshot::Timer::new(std::time::Duration::from_millis(50)).await;
                log::info!("asys passed timer");

                data.1.send(data.0).await?;
            }
            Ok(())
        }

        fn sys(mut data: WriteExpect<DataOne>) -> anyhow::Result<ShouldContinue> {
            log::info!("running sys");
            data.0 += 1;
            ok()
        }

        let (tx, rx) = mpsc::bounded(1);
        let mut world = World::default();
        world
            .with_resource(DataOne(0, tx))
            .unwrap()
            .with_async_system("asys", asys)
            .with_system("sys", sys)
            .unwrap();
        // it takes two ticks for asys to run:
        // 1. execute up to the first await point
        // 2. deliver resources and compute the rest
        world.tick().unwrap();
        world.tick().unwrap();
        assert_eq!(Some(1), rx.try_recv().ok());
    }

    #[test]
    fn can_create_entities_and_build_convenience() -> anyhow::Result<()> {
        struct DataA(f32);
        impl StoredComponent for DataA {
            type StorageType = VecStorage<Self>;
        }

        struct DataB(f32);
        impl StoredComponent for DataB {
            type StorageType = VecStorage<Self>;
        }

        let mut world = World::default();
        assert!(world.has_resource::<Entities>(), "missing entities");
        let _entity = world
            .entity()
            .unwrap()
            .with(DataA(0.0))
            .unwrap()
            .with(DataB(0.0))
            .unwrap()
            .build();

        Ok(())
    }

    #[test]
    fn plugin_inserts_resources_from_canfetch_in_systems() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world
            .with_system("test", |_: Write<&'static str>| ok())
            .unwrap();
        let s = world.resource_get_mut::<&'static str>().unwrap();
        *s = "blah";
    }
}
