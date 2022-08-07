//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::any::Any;
use std::future::Future;
use std::sync::Arc;
use std::{iter::Map, ops::Deref};

use anyhow::Context;
use async_oneshot::oneshot;
use hibitset::{BitIter, BitSet, BitSetLike};
use rustc_hash::FxHashSet;

use crate::resource_manager::{LoanManager, ResourceManager};
use crate::schedule::UntypedSystemData;
use crate::storage::separated::{ReadStore, SeparateStorageExt, VecStorage, WriteStore};
use crate::{
    mpsc,
    plugins::Plugin,
    schedule::IsSchedule,
    spsc,
    system::{
        AsyncSchedule, AsyncSystem, AsyncSystemFuture, AsyncSystemRequest, ShouldContinue,
        SyncSchedule, SyncSystem,
    },
    CanFetch, IsResource, Request, Resource, ResourceId, ResourceRequirement,
};
use crate::{oneshot, WriteExpect};

/// Fetches world resources in async systems.
pub struct Facade {
    // Unbounded. Sending a request from the system should not yield the async
    pub(crate) resource_request_tx: spsc::Sender<Request>,
    // Bounded(1). Awaiting in the system should yield the async
    pub(crate) resources_to_system_rx: spsc::Receiver<Resource>,
}

impl Facade {
    pub async fn fetch<T: CanFetch + Send + Sync + 'static>(&mut self) -> anyhow::Result<T> {
        let borrows = T::borrows();
        self.resource_request_tx
            .try_send(Request {
                borrows,
                construct: |loan_mngr: &mut LoanManager| {
                    let t = T::construct(loan_mngr).with_context(|| {
                        format!("could not construct {}", std::any::type_name::<T>())
                    })?;
                    let box_t: Box<T> = Box::new(t);
                    let box_any: UntypedSystemData = box_t;
                    Ok(box_any)
                },
            })
            .context("could not send request for resources")?;

        let box_any: Resource = self
            .resources_to_system_rx
            .recv()
            .await
            .context("could not fetch resources")?;
        let box_t: Box<T> = box_any
            .downcast()
            .ok()
            .with_context(|| format!("Facade could not downcast {}", std::any::type_name::<T>()))?;
        let t = *box_t;
        Ok(t)
    }
}

pub struct Lazy(mpsc::Sender<LazyOp>);

impl Lazy {
    pub fn exec_mut<T: Any + Send + Sync>(
        &self,
        op: impl FnOnce(&mut World) -> anyhow::Result<T> + Send + Sync + 'static,
    ) -> anyhow::Result<impl Future<Output = anyhow::Result<T>>> {
        let (tx, rx) = oneshot();
        self.0
            .try_send(LazyOp {
                op: Box::new(|world| {
                    let t = op(world)?;
                    let arc_t = Arc::new(t);
                    Ok(arc_t)
                }),
                tx,
            })
            .context("could not send lazy op")?;
        Ok(async move {
            let arc: Arc<dyn Any + Send + Sync> = rx
                .await
                .map_err(|_| anyhow::anyhow!("lazy exec_mut oneshot is closed"))?;
            let t: Arc<T> = arc.downcast::<T>().map_err(|_| {
                anyhow::anyhow!("could not downcast to {}", std::any::type_name::<T>())
            })?;

            Arc::try_unwrap(t).map_err(|_| anyhow::anyhow!("could not unwrap arc"))
        })
    }
}

pub struct LazyOp {
    op: Box<
        dyn FnOnce(&mut World) -> anyhow::Result<Arc<dyn Any + Send + Sync>>
            + Send
            + Sync
            + 'static,
    >,
    tx: oneshot::Sender<Arc<dyn Any + Send + Sync>>,
}

pub struct Entity {
    id: usize,
    gen: usize,
    op_sender: mpsc::Sender<LazyOp>,
    op_receivers: Vec<oneshot::Receiver<Arc<dyn Any + Send + Sync>>>,
}

/// You may clone entities, but each one does its own lazy updates,
/// separate from all other clones.
impl Clone for Entity {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            gen: self.gen.clone(),
            op_sender: self.op_sender.clone(),
            op_receivers: Default::default(),
        }
    }
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
    pub fn lazy_with<T: Send + Sync + 'static>(mut self, component: T) -> anyhow::Result<Self> {
        let id = self.id;
        let (tx, rx) = oneshot();
        let op = Box::new(move |world: &mut World| {
            if !world.has_resource::<VecStorage<T>>() {
                world.with_default_storage::<T>()?;
            }
            let mut storage: WriteStore<T> = world.fetch()?;
            let _ = storage.insert(id, component);
            Ok(Arc::new(()) as Arc<dyn Any + Send + Sync>)
        });
        self.op_sender
            .try_send(LazyOp { op, tx })
            .context("could not send entity op")?;
        self.op_receivers.push(rx);
        Ok(self)
    }

    /// Lazily add a component and return a future that completes after the
    /// component has been added.
    pub fn lazy_add<T: Send + Sync + 'static>(&mut self, component: T) -> anyhow::Result<()> {
        let id = self.id;
        let (tx, rx) = oneshot();
        let op = LazyOp {
            op: Box::new(move |world: &mut World| {
                if !world.has_resource::<VecStorage<T>>() {
                    world.with_default_storage::<T>()?;
                }
                let mut storage: WriteStore<T> = world.fetch()?;
                let _ = storage.insert(id, component);
                Ok(Arc::new(()) as Arc<dyn Any + Send + Sync>)
            }),
            tx,
        };
        self.op_sender
            .try_send(op)
            .context("could not send entity op")?;
        self.op_receivers.push(rx);
        Ok(())
    }

    /// Await a future that completes after all lazy updates have been
    /// performed.
    pub async fn updates(&mut self) -> anyhow::Result<()> {
        let updates: Vec<oneshot::Receiver<Arc<dyn Any + Send + Sync>>> =
            std::mem::take(&mut self.op_receivers);
        for update in updates.into_iter() {
            let _ = update
                .await
                .map_err(|_| anyhow::anyhow!("updates oneshot is closed"))?;
        }
        Ok(())
    }

    /// Get the value of a specific component, if it exists.
    pub async fn lazy_get<T: Clone + Send + Sync + 'static>(&self) -> anyhow::Result<Option<T>> {
        let id = self.id();
        let (tx, rx) = oneshot();
        self.op_sender
            .try_send(LazyOp {
                op: Box::new(move |world: &mut World| {
                    if !world.has_resource::<VecStorage<T>>() {
                        world.with_default_storage::<T>()?;
                    }
                    let storage: ReadStore<T> = world.fetch()?;
                    Ok(Arc::new(storage.get(id).map(Clone::clone)) as Arc<dyn Any + Send + Sync>)
                }),
                tx,
            })
            .context("could not send entity op")?;
        let arc: Arc<dyn Any + Send + Sync> = rx
            .await
            .map_err(|_| anyhow::anyhow!("could not receive get request"))?;
        let arc_c: Arc<Option<T>> = arc.downcast().map_err(|_| {
            anyhow::anyhow!(
                "could not
     downcast"
            )
        })?;
        let c: Option<T> =
            Arc::try_unwrap(arc_c).map_err(|_| anyhow::anyhow!("could not unwrap"))?;
        Ok(c)
    }
}

pub struct SeparateEntityBuilder<'a> {
    entity: Entity,
    world: &'a mut World,
}

impl<'a> SeparateEntityBuilder<'a> {
    /// Adds a component immediately.
    pub fn with<T: Send + Sync + 'static>(self, component: T) -> anyhow::Result<Self> {
        self.world
            .resource_manager
            .unify_resources("SeparateEntityBuilder::with")?;

        if !self.world.has_resource::<VecStorage<T>>() {
            self.world.with_default_storage::<T>()?;
        }
        let mut storage: WriteStore<T> = self.world.fetch()?;
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
    pub dead: Vec<Entity>,
    pub recycle: Vec<Entity>,
    pub lazy_op_sender: mpsc::Sender<LazyOp>,
}

impl Default for Entities {
    fn default() -> Self {
        Self {
            next_k: Default::default(),
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
            dead: Default::default(),
            recycle: Default::default(),
            lazy_op_sender,
        }
    }

    pub fn create(&mut self) -> Entity {
        self.recycle.pop().unwrap_or_else(|| {
            let id = self.next_k;
            self.next_k += 1;
            Entity {
                id,
                gen: 0,
                op_sender: self.lazy_op_sender.clone(),
                op_receivers: Default::default(),
            }
        })
    }

    pub fn destroy(&mut self, mut entity: Entity) {
        entity.op_receivers = Default::default();
        self.dead.push(entity);
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
            resource_manager: ResourceManager::default(),
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
    name: String,
    make_system_future: F,
) -> (AsyncSystem, AsyncSystemFuture)
where
    F: FnOnce(Facade) -> Fut,
    Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
{
    let (resource_request_tx, resource_request_rx) = spsc::unbounded();
    let (resources_to_system_tx, resources_to_system_rx) = spsc::bounded(1);
    let facade = Facade {
        resource_request_tx,
        resources_to_system_rx,
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

    pub fn set_resource<T: IsResource>(&mut self, resource: T) -> anyhow::Result<Option<T>> {
        if let Some(prev) = self.resource_manager.add(resource) {
            match prev.downcast::<T>() {
                Ok(t) => Ok(Some(*t)),
                Err(_) => Err(anyhow::anyhow!("could not downcast previous resource")),
            }
        } else {
            Ok(None)
        }
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

            let (sys, fut) = make_async_system_pack(asystem.0, asystem.1);
            self.add_async_system_pack(sys, fut);
        }

        Ok(self)
    }

    pub fn with_data<T: CanFetch>(&mut self) -> anyhow::Result<&mut Self> {
        self.with_plugin(T::plugin())
    }

    pub fn entity(&mut self) -> anyhow::Result<SeparateEntityBuilder<'_>> {
        let mut entities = self.fetch::<WriteExpect<Entities>>()?;
        let entity = entities.create();
        Ok(SeparateEntityBuilder {
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
        T: CanFetch + 'static,
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
        T: CanFetch + 'static,
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
                    panic!("async system '{}' erred: {}", name, msg);
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
        let (system, fut) = make_async_system_pack(name.as_ref().to_string(), make_system_future);
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
    /// Calls `World::tick_async`, then `World::tick_sync`, then
    /// `World::tick_lazy`.
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
        log::trace!(
            "execution:\n{}",
            self.sync_schedule.get_execution_order().join("\n")
        );
        self.sync_schedule.run((), &mut self.resource_manager)?;

        self.resource_manager.unify_resources("tick sync")?;
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
                    if let Some(request) = system.resource_request_rx.try_recv().ok() {
                        schedule.add_system(AsyncSystemRequest(&system, request));
                    }
                    schedule
                });

        if !schedule.is_empty() {
            log::trace!(
                "async system execution:\n{}",
                schedule.get_execution_order().join("\n")
            );
            schedule.run(&self.async_system_executor, &mut self.resource_manager)?;
        } else {
            // do one tick anyway, because most async systems don't require scheduling
            // (especially the first frame)
            let _ = self.async_system_executor.try_tick();
        }

        // lastly tick all our non-system tasks
        if !self.async_task_executor.is_empty() {
            let mut ticks = 0;
            while self.async_task_executor.try_tick() {
                ticks += 1;
            }
            log::trace!("ticked {} futures", ticks);
        }

        self.resource_manager.unify_resources("tick async")?;
        Ok(())
    }

    /// Applies lazy world updates.
    pub fn tick_lazy(&mut self) -> anyhow::Result<()> {
        log::trace!("tick lazy");

        while let Ok(LazyOp { op, mut tx }) = self.lazy_ops.1.try_recv() {
            self.resource_manager.unify_resources("World::tick_lazy")?;
            let t = (op)(self)?;
            let _ = tx.send(t);
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
        self.resource_manager.get_mut::<T>()
    }

    pub fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        self.resource_manager.unify_resources("World::fetch")?;
        T::construct(&mut LoanManager(&mut self.resource_manager))
    }

    /// Run all system and non-system futures until they have all finished or
    /// one system has erred, whichever comes first.
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
    use std::{ops::DerefMut, sync::Mutex};

    use crate::{
        self as apecs,
        storage::archetype::{AllArchetypes, Query},
    };
    use apecs::{
        anyhow, spsc, storage::separated::*, system::*, world::*, Read, Write, WriteExpect,
    };
    use rustc_hash::FxHashMap;

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
    fn async_systems_run_and_return_resources() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        async fn create(tx: spsc::Sender<()>, mut facade: Facade) -> anyhow::Result<()> {
            log::info!("create running");
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
            .with_system("maintain", maintain_map)
            .unwrap();

        // create system runs - sending on the channel and making the fetch request
        world.tick().unwrap();
        rx.try_recv().unwrap();

        // world sends resources to the create system which makes
        // entities+components, then the maintain system updates the book
        world.tick().unwrap();

        let book = world.fetch::<Read<FxHashMap<String, u32>>>().unwrap();
        for n in 0..100 {
            assert_eq!(book.get(&format!("entity_{}", n)), Some(&n));
        }
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

                data.deref().1.send(data.deref().0).await?;
            }
            Ok(())
        }

        fn sys(mut data: WriteExpect<DataOne>) -> anyhow::Result<ShouldContinue> {
            log::info!("running sys");
            data.deref_mut().0 += 1;
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
    fn can_create_entities_and_build_convenience() {
        struct DataA(f32);
        struct DataB(f32);

        let mut world = World::default();
        assert!(world.has_resource::<Entities>(), "missing entities");
        let e = world
            .entity()
            .unwrap()
            .with(DataA(0.0))
            .unwrap()
            .with(DataB(0.0))
            .unwrap()
            .build();
        let id = e.id();

        world.with_async(async move {
            println!("updating entity");
            e.lazy_with(DataA(666.0))
                .unwrap()
                .lazy_with(DataB(666.0))
                .unwrap()
                .updates()
                .await
                .unwrap();
            println!("done!");
        });

        while !world.async_task_executor.is_empty() {
            world.tick().unwrap();
        }

        let (a_store, b_store): (ReadStore<DataA>, ReadStore<DataB>) = world.fetch().unwrap();
        let a = a_store.get(id).unwrap();
        assert_eq!(a.0, 666.0);

        let b = b_store.get(id).unwrap();
        assert_eq!(b.0, 666.0);
    }

    #[test]
    fn entities_can_lazy_add_and_get() {
        #[derive(Debug, Clone, PartialEq)]
        struct Name(&'static str);

        #[derive(Debug, Clone, PartialEq)]
        struct Age(u32);

        // this tests that we can use an Entity to set components and
        // await those component updates, as well as that ticking the
        // world advances tho async system up to exactly one await
        // point per tick.
        let await_points = Arc::new(Mutex::new(vec![]));
        let awaits = await_points.clone();
        let mut world = World::default();
        world.with_async_system("test", |mut facade| async move {
            await_points.lock().unwrap().push(1);
            let mut e = {
                let mut entities: Write<Entities> = facade.fetch().await?;
                await_points.lock().unwrap().push(2);
                entities.create()
            };

            e.lazy_add(Name("ada")).unwrap();
            e.lazy_add(Age(666)).unwrap();
            e.updates().await.unwrap();
            await_points.lock().unwrap().push(3);

            let name = e.lazy_get::<Name>().await.unwrap();
            await_points.lock().unwrap().push(4);
            assert_eq!(Some(Name("ada")), name);

            let age = e.lazy_get::<Age>().await.unwrap();
            await_points.lock().unwrap().push(5);
            assert_eq!(Some(Age(666)), age);

            println!("done!");
            Ok(())
        });

        for i in 1..=5 {
            world.tick().unwrap();
            assert!(awaits.lock().unwrap().last().cloned() == Some(i));
        }

        let store = world.fetch::<ReadStore<Age>>().unwrap();
        let age = store.get(0).unwrap();
        assert_eq!(&Age(666), age);
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

    #[test]
    fn sanity_channel_ref() {
        let f = 0.0f32;
        let (tx, rx) = mpsc::unbounded();
        tx.try_send(&f).unwrap();
        assert_eq!(&0.0, rx.try_recv().unwrap());
    }

    #[test]
    fn can_query_empty_ref_archetypes_in_same_batch() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world
            .with_resource(AllArchetypes::default())
            .unwrap()
            .with_system("one", |query: Query<(&f32, &bool)>| {
                query.for_each(|(_f, _b)| {});
                ok()
            })
            .unwrap()
            .with_system("two", |query: Query<(&f32, &bool)>| {
                query.for_each(|(_f, _b)| {});
                ok()
            })
            .unwrap();
        world.tick().unwrap();
    }

    #[test]
    fn can_query_ref_archetypes_in_same_batch() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut all = AllArchetypes::default();
        all.insert_bundle(0, (0.0f32, true));
        all.insert_bundle(1, (1.0f32, false));

        let mut world = World::default();
        world
            .with_resource(all)
            .unwrap()
            .with_system("one", |query: Query<(&f32, &bool)>| {
                let mut q = vec![];
                query.for_each(|(f, b)| q.push((f.id(), **f, **b)));
                assert_eq!(vec![(0, 0.0, true), (1, 1.0, false)], q);
                ok()
            })
            .unwrap()
            .with_system("two", |query: Query<(&f32, &bool)>| {
                let mut q = vec![];
                query.for_each(|(f, b)| q.push((f.id(), **f, **b)));
                assert_eq!(vec![(0, 0.0, true), (1, 1.0, false)], q);
                ok()
            })
            .unwrap();
        world.tick().unwrap();
    }
}
