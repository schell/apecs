//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::any::TypeId;
use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;
use std::{any::Any, collections::VecDeque};

use anyhow::Context;
use rustc_hash::FxHashSet;

use crate::QueryGuard;
use crate::{
    chan::{self, mpsc, oneshot::oneshot, spsc},
    plugin::Plugin,
    resource_manager::{LoanManager, ResourceManager},
    schedule::{Dependency, IsSchedule, UntypedSystemData},
    storage::{Components, Entry, IsBundle, IsQuery},
    system::{
        AsyncSchedule, AsyncSystem, AsyncSystemFuture, AsyncSystemRequest, ShouldContinue,
        SyncSchedule, SyncSystem,
    },
    CanFetch, IsResource, LazyResource, Request, Resource, ResourceId, Write,
};

/// Fetches world resources in async systems.
///
/// A facade is a window into the world, by which an async system can interact
/// with the world without causing resource contention.
pub struct Facade {
    // Unbounded. Sending a request from the system should not yield the async
    pub(crate) resource_request_tx: spsc::Sender<Request>,
    // Bounded(1). Awaiting in the system should yield the async
    pub(crate) resources_to_system_rx: spsc::Receiver<Resource>,
}

impl Facade {
    /// Asyncronously visit fetchable system resources using a closure.
    ///
    /// The closure may return data to the caller.
    ///
    /// A roundtrip takes one frame.
    ///
    /// Using a closure ensures that no fetched system resources are held over
    /// an await point, which would preclude other systems from accessing
    /// them and susequently being able to run.
    pub async fn visit<D: CanFetch + Send + Sync + 'static, T: Send + Sync + 'static>(
        &mut self,
        f: impl FnOnce(D) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let borrows = D::borrows();
        self.resource_request_tx
            .try_send(Request {
                borrows,
                construct: |loan_mngr: &mut LoanManager| {
                    let t = D::construct(loan_mngr).with_context(|| {
                        format!("could not construct {}", std::any::type_name::<D>())
                    })?;
                    let box_t: Box<D> = Box::new(t);
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
        let box_d: Box<D> = box_any
            .downcast()
            .ok()
            .with_context(|| format!("Facade could not downcast {}", std::any::type_name::<D>()))?;
        let d = *box_d;
        let t = f(d)?;
        Ok(t)
    }
}

pub struct LazyOp {
    op: Box<
        dyn FnOnce(&mut World) -> anyhow::Result<Arc<dyn Any + Send + Sync>>
            + Send
            + Sync
            + 'static,
    >,
    tx: chan::oneshot::Sender<Arc<dyn Any + Send + Sync>>,
}

/// Associates a group of components within the world.
///
/// Entities are mostly just `usize` identifiers that help locate components,
/// but [`Entity`] comes with some conveniences.
///
/// After an entity is created you can attach and remove
/// bundles of components asynchronously (or "lazily" if not in an async
/// context).
/// ```
/// # use apecs::*;
/// let mut world = World::default();
/// // asynchronously create new entities from within an async system
/// world.with_async_system("spawn-b-entity", |mut facade: Facade| async move {
///     let mut b = facade
///         .visit(|mut ents: Write<Entities>| {
///             Ok(ents.create().with_bundle((123, "entity B", true)))
///         })
///         .await?;
///     // after this await `b` has its bundle
///     b.updates().await;
///     // visit a component or bundle
///     let did_visit = b
///         .visit::<&&str, ()>(|name| assert_eq!("entity B", *name.value()))
///         .await
///         .is_some();
///     Ok(())
/// });
/// world.run();
/// ```
/// Alternatively, if you have access to the [`Components`] resource you can
/// attach components directly and immediately.
pub struct Entity {
    id: usize,
    gen: usize,
    op_sender: mpsc::Sender<LazyOp>,
    op_receivers: Vec<chan::oneshot::Receiver<Arc<dyn Any + Send + Sync>>>,
}

impl std::fmt::Debug for Entity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entity")
            .field("id", &self.id)
            .field("gen", &self.gen)
            .finish()
    }
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

    /// Lazily add a component bundle to archetype storage.
    ///
    /// This entity will have the associated components after the next tick.
    pub fn with_bundle<B: IsBundle + Send + Sync + 'static>(mut self, bundle: B) -> Self {
        self.insert_bundle(bundle);
        self
    }

    /// Lazily add a component bundle to archetype storage.
    ///
    /// This entity will have the associated components after the next tick.
    pub fn insert_bundle<B: IsBundle + Send + Sync + 'static>(&mut self, bundle: B) {
        let id = self.id;
        let (tx, rx) = oneshot();
        let op = LazyOp {
            op: Box::new(move |world: &mut World| {
                if !world.has_resource::<Components>() {
                    world.with_resource(Components::default()).unwrap();
                }
                let all: &mut Components = world.resource_mut()?;
                let _ = all.insert_bundle(id, bundle);
                Ok(Arc::new(()) as Arc<dyn Any + Send + Sync>)
            }),
            tx,
        };
        self.op_sender
            .try_send(op)
            .expect("could not send entity op");
        self.op_receivers.push(rx);
    }

    /// Lazily add a component bundle to archetype storage.
    ///
    /// This entity will have the associated component after the next tick.
    pub fn insert_component<T: Send + Sync + 'static>(&mut self, component: T) {
        let id = self.id;
        let (tx, rx) = oneshot();
        let op = LazyOp {
            op: Box::new(move |world: &mut World| {
                if !world.has_resource::<Components>() {
                    world.with_resource(Components::default()).unwrap();
                }
                let all: &mut Components = world.resource_mut()?;
                let _ = all.insert_component(id, component);
                Ok(Arc::new(()) as Arc<dyn Any + Send + Sync>)
            }),
            tx,
        };
        self.op_sender
            .try_send(op)
            .expect("could not send entity op");
        self.op_receivers.push(rx);
    }

    /// Lazily remove a component bundle to archetype storage.
    ///
    /// This entity will have lost the associated component after the next tick.
    pub fn remove_component<T: Send + Sync + 'static>(&mut self) {
        let id = self.id;
        let (tx, rx) = oneshot();
        let op = LazyOp {
            op: Box::new(move |world: &mut World| {
                if !world.has_resource::<Components>() {
                    world.with_resource(Components::default()).unwrap();
                }
                let all: &mut Components = world.resource_mut()?;
                let _ = all.remove_component::<T>(id);
                Ok(Arc::new(()) as Arc<dyn Any + Send + Sync>)
            }),
            tx,
        };
        self.op_sender
            .try_send(op)
            .expect("could not send entity op");
        self.op_receivers.push(rx);
    }

    /// Await a future that completes after all lazy updates have been
    /// performed.
    pub async fn updates(&mut self) {
        let updates: Vec<chan::oneshot::Receiver<Arc<dyn Any + Send + Sync>>> =
            std::mem::take(&mut self.op_receivers);
        for update in updates.into_iter() {
            let _ = update
                .await
                .map_err(|_| anyhow::anyhow!("updates oneshot is closed"))
                .unwrap();
        }
    }

    /// Visit this entity's query row in storage, if it exists.
    ///
    /// ## Panics
    /// Panics if the query is malformed (the bundle is not unique).
    pub async fn visit<Q: IsQuery + 'static, T: Send + Sync + 'static>(
        &self,
        f: impl FnOnce(Q::QueryRow<'_>) -> T + Send + Sync + 'static,
    ) -> Option<T> {
        let id = self.id();
        let (tx, rx) = oneshot();
        self.op_sender
            .try_send(LazyOp {
                op: Box::new(move |world: &mut World| {
                    if !world.has_resource::<Components>() {
                        world.with_resource(Components::default())?;
                    }
                    let mut storage: Write<Components> = world.fetch()?;
                    let mut q = storage.query::<Q>();
                    Ok(Arc::new(q.find_one(id).map(f)) as Arc<dyn Any + Send + Sync>)
                }),
                tx,
            })
            .context("could not send entity op")
            .unwrap();
        let arc: Arc<dyn Any + Send + Sync> = rx
            .await
            .map_err(|_| anyhow::anyhow!("could not receive get request"))
            .unwrap();
        let arc_c: Arc<Option<T>> = arc
            .downcast()
            .map_err(|_| anyhow::anyhow!("could not downcast"))
            .unwrap();
        let c: Option<T> = Arc::try_unwrap(arc_c)
            .map_err(|_| anyhow::anyhow!("could not unwrap"))
            .unwrap();
        c
    }
}

/// Creates, destroys and recycles entities.
///
/// The most common reason to interact with `Entities` is to
/// [`create`](Entities::create) or [`destroy`](Entities::destroy) an
/// [`Entity`].
/// ```
/// # use apecs::*;
/// let mut world = World::default();
/// let entities = world.resource_mut::<Entities>().unwrap();
/// let ent = entities.create();
/// entities.destroy(ent);
/// ```
pub struct Entities {
    pub(crate) next_k: usize,
    pub(crate) generations: Vec<usize>,
    pub(crate) recycle: Vec<usize>,
    pub(crate) delete_tx: spsc::Sender<usize>,
    pub(crate) delete_rx: spsc::Receiver<usize>,
    pub(crate) deleted: VecDeque<(u64, Vec<(usize, smallvec::SmallVec<[TypeId; 4]>)>)>,
    pub(crate) lazy_op_sender: mpsc::Sender<LazyOp>,
}

impl Default for Entities {
    fn default() -> Self {
        let (delete_tx, delete_rx) = spsc::unbounded();
        Self {
            next_k: Default::default(),
            generations: vec![],
            recycle: Default::default(),
            delete_rx,
            delete_tx,
            deleted: Default::default(),
            lazy_op_sender: mpsc::unbounded().0,
        }
    }
}

impl Entities {
    pub fn new(lazy_op_sender: mpsc::Sender<LazyOp>) -> Self {
        Self {
            lazy_op_sender,
            ..Default::default()
        }
    }

    fn dequeue(&mut self) -> usize {
        if let Some(id) = self.recycle.pop() {
            self.generations[id] += 1;
            id
        } else {
            let id = self.next_k;
            self.generations.push(0);
            self.next_k += 1;
            id
        }
    }

    /// Returns the number of entities that are currently alive.
    pub fn alive_len(&self) -> usize {
        self.generations.len() - self.recycle.len()
    }

    /// Create many entities at once, returning a list of their ids.
    ///
    /// An [`Entity`] can be made from its `usize` id using `Entities::hydrate`.
    pub fn create_many(&mut self, mut how_many: usize) -> Vec<usize> {
        let mut ids = vec![];
        while let Some(id) = self.recycle.pop() {
            self.generations[id] += 1;
            ids.push(id);
            how_many -= 1;
            if how_many == 0 {
                return ids;
            }
        }

        let last_id = self.next_k + (how_many - 1);
        self.generations.resize_with(last_id, || 0);
        ids.extend(self.next_k..=last_id);
        self.next_k = last_id + 1;
        ids
    }

    /// Create one entity and return it.
    pub fn create(&mut self) -> Entity {
        let id = self.dequeue();
        Entity {
            id,
            gen: self.generations[id],
            op_sender: self.lazy_op_sender.clone(),
            op_receivers: Default::default(),
        }
    }

    /// Lazily destroy an entity, removing its components and recycling it
    /// at the end of this tick.
    ///
    /// ## NOTE:
    /// Destroyed entities will have their components removed
    /// automatically during upkeep, which happens each `World::tick_lazy`.
    pub fn destroy(&self, mut entity: Entity) {
        entity.op_receivers = Default::default();
        self.delete_tx.try_send(entity.id()).unwrap();
    }

    /// Produce an iterator of deleted entities as entries.
    ///
    /// This iterator should be filtered at the callsite for the latest changed
    /// entries since a stored iteration timestamp.
    pub fn deleted_iter(&self) -> impl Iterator<Item = Entry<()>> + '_ {
        self.deleted.iter().flat_map(|(changed, ids)| {
            ids.iter().map(|(id, _)| Entry {
                value: (),
                key: *id,
                changed: *changed,
                added: true,
            })
        })
    }

    /// Produce an iterator of deleted entities that had a component of the
    /// given type, as entries.
    ///
    /// This iterator should be filtered at the callsite for the latest changed
    /// entries since a stored iteration timestamp.
    pub fn deleted_iter_of<T: 'static>(&self) -> impl Iterator<Item = Entry<()>> + '_ {
        let ty = TypeId::of::<Entry<T>>();
        self.deleted.iter().flat_map(move |(changed, ids)| {
            ids.iter().filter_map(move |(id, tys)| {
                if tys.contains(&ty) {
                    Some(Entry {
                        value: (),
                        key: *id,
                        changed: *changed,
                        added: true,
                    })
                } else {
                    None
                }
            })
        })
    }

    /// Hydrate an `Entity` from an id.
    pub fn hydrate(&self, entity_id: usize) -> Option<Entity> {
        let gen = self.generations.get(entity_id)?;
        Some(Entity {
            id: entity_id,
            gen: *gen,
            op_sender: self.lazy_op_sender.clone(),
            op_receivers: Default::default(),
        })
    }

    /// Hydrate the entity with the given id and then lazily add the given
    /// bundle to the entity.
    ///
    /// This is a noop if the entity does not exist.
    pub fn insert_bundle<B: IsBundle + Send + Sync + 'static>(&self, entity_id: usize, bundle: B) {
        if let Some(mut entity) = self.hydrate(entity_id) {
            entity.insert_bundle(bundle);
        }
    }

    /// Hydrate the entity with the given id and then lazily add the given
    /// component.
    ///
    /// This is a noop if the entity does not exist.
    pub fn insert_component<T: Send + Sync + 'static>(&self, entity_id: usize, component: T) {
        if let Some(mut entity) = self.hydrate(entity_id) {
            entity.insert_component(component);
        }
    }

    /// Hydrate the entity with the given id and then lazily remove the
    /// component of the given type.
    ///
    /// This is a noop if the entity does not exist.
    pub fn remove_component<T: Send + Sync + 'static>(&self, entity_id: usize) {
        if let Some(mut entity) = self.hydrate(entity_id) {
            entity.remove_component::<T>();
        }
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

/// Defines the number of threads to use for inner and outer parallelism.
pub enum Parallelism {
    Automatic,
    Explicit(u32),
}

/// A collection of resources and systems.
///
/// The `World` is the executor of your systems and async computations.
///
/// Most applications will create and configure a `World` in their main
/// function, finally [`run`](World::run)ning it or [`tick`](World::tick)ing
/// it in a loop.
///
/// ```rust
/// use apecs::*;
///
/// #[derive(CanFetch)]
/// struct MyAppData {
///     channel: Read<chan::mpmc::Channel<String>>,
///     count: Write<usize>,
/// }
///
/// let mut world = World::default();
/// world
///     .with_system(
///         "compute_hello",
///         |mut data: MyAppData| -> anyhow::Result<ShouldContinue> {
///             if *data.count >= 3 {
///                 data.channel
///                     .tx
///                     .try_broadcast("hello world".to_string())
///                     .unwrap();
///                 end()
///             } else {
///                 *data.count += 1;
///                 ok()
///             }
///         },
///     )
///     .unwrap()
///     .with_async_system("await_hello", |mut facade: Facade| async move {
///         let mut rx = facade
///             .visit(|mut data: MyAppData| Ok(data.channel.new_receiver()))
///             .await?;
///         if let Ok(msg) = rx.recv().await {
///             println!("got message: {}", msg);
///         }
///         Ok(())
///     });
/// world.run();
/// ```
///
/// `World` is the outermost object of `apecs`, as it contains all systems and
/// resources. It contains [`Entities`] and [`Components`] as default resources.
/// You can create entities, attach components and query them from outside the
/// world if desired:
///
/// ```
/// # use apecs::*;
/// let mut world = World::default();
/// // Nearly any type can be used as a component with zero boilerplate
/// let a = world.entity_with_bundle((123, true, "abc"));
/// let b = world.entity_with_bundle((42, false));
///
/// // Query the world for all matching bundles
/// let mut query = world.query::<(&mut i32, &bool)>();
/// for (number, flag) in query.iter_mut() {
///     if **flag {
///         **number *= 2;
///     }
/// }
///
/// // Perform random access within the same query
/// let a_entry: &Entry<i32> = query.find_one(a.id()).unwrap().0;
/// assert_eq!(**a_entry, 246);
/// // Track changes to individual components
/// assert_eq!(apecs::current_iteration(), a_entry.last_changed());
/// ```
///
/// ## Where to look next 📚
/// * [`Entities`] for info on creating and deleting [`Entity`]s
/// * [`Components`] for info on creating updating and deleting components
/// * [`Entry`] for info on tracking changes to individual components
/// * [`Query`](crate::Query) for info on querying bundles of components
/// * [`Facade`] for info on interacting with the `World` from within an async
///   system
/// * [`Plugin`] for info on how to bundle systems and resources of common
///   operation together into an easy-to-integrate package
pub struct World {
    pub(crate) resource_manager: ResourceManager,
    pub(crate) sync_schedule: SyncSchedule,
    pub(crate) async_systems: Vec<AsyncSystem>,
    pub(crate) async_system_executor: async_executor::Executor<'static>,
    // executor for non-system futures
    pub(crate) async_task_executor: async_executor::Executor<'static>,
    pub(crate) lazy_ops: (mpsc::Sender<LazyOp>, mpsc::Receiver<LazyOp>),
}

impl Default for World {
    fn default() -> Self {
        let lazy_ops = mpsc::unbounded();
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
            .with_resource(entities)
            .unwrap()
            .with_resource(Components::default())
            .unwrap();
        world
    }
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

    /// Add a plugin to the world, instantiating any missing resources or
    /// systems.
    ///
    /// ## Errs
    /// Errs if the plugin requires resources that cannot be created by default.
    pub fn with_plugin(&mut self, plugin: impl Into<Plugin>) -> anyhow::Result<&mut Self> {
        let plugin = plugin.into();

        let mut missing_required_resources: FxHashSet<ResourceId> = FxHashSet::default();
        for LazyResource { id, create } in plugin.resources.into_iter() {
            if !self.resource_manager.has_resource(&id) {
                log::trace!("attempting to create missing resource {}...", id.name);
                let resource = (create)(&mut self.resource_manager.as_mut_loan_manager())?;
                missing_required_resources.remove(&id);
                let _ = self.resource_manager.insert(id, resource);
                self.resource_manager
                    .unify_resources("after building lazy dep")?;
            }
        }

        anyhow::ensure!(
            missing_required_resources.is_empty(),
            "missing required resources:\n{:#?}",
            missing_required_resources
        );

        for system in plugin.sync_systems.into_iter() {
            if !self.sync_schedule.contains_system(&system.0.name) {
                self.sync_schedule.add_system(system.0);
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

    /// Add a syncronous system.
    ///
    /// ## Errs
    /// Errs if expected resources must first be added to the world.
    pub fn with_system<T, F>(
        &mut self,
        name: impl AsRef<str>,
        sys_fn: F,
    ) -> anyhow::Result<&mut Self>
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch + 'static,
    {
        self.with_system_with_dependencies(name, sys_fn, &[], &[])
    }

    /// Add a syncronous system that has a dependency on one or more other
    /// syncronous systems.
    ///
    /// ## Errs
    /// Errs if expected resources must first be added to the world.
    pub fn with_system_with_dependencies<T, F>(
        &mut self,
        name: impl AsRef<str>,
        sys_fn: F,
        after_deps: &[&str],
        before_deps: &[&str],
    ) -> anyhow::Result<&mut Self>
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch + 'static,
    {
        let mut deps = after_deps
            .iter()
            .map(|dep| Dependency::After(dep.to_string()))
            .collect::<Vec<_>>();
        deps.extend(
            before_deps
                .iter()
                .map(|dep| Dependency::Before(dep.to_string())),
        );
        let system = SyncSystem::new(name, sys_fn, deps);

        self.with_plugin(T::plugin())?;
        self.sync_schedule.add_system(system);
        Ok(self)
    }

    /// Add a syncronous system barrier.
    ///
    /// Any systems added after the barrier will be scheduled after the systems
    /// added before the barrier.
    pub fn with_system_barrier(&mut self) -> &mut Self {
        self.sync_schedule.add_barrier();
        self
    }

    pub fn with_parallelism(&mut self, parallelism: Parallelism) -> &mut Self {
        let num_threads = match parallelism {
            Parallelism::Automatic => {
                #[cfg(target_arch = "wasm32")]
                {
                    1
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    rayon::current_num_threads() as u32
                }
            }
            Parallelism::Explicit(n) => {
                if n > 1 {
                    log::info!("building a rayon thread pool with {} threads", n);
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(n as usize)
                        .build()
                        .unwrap();
                    n
                } else {
                    1
                }
            }
        };
        self.sync_schedule.set_parallelism(num_threads);
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

    /// Spawn an asynchronous task.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + Sync + 'static) {
        let task = self.async_task_executor.spawn(future);
        task.detach();
    }

    /// Conduct a world tick.
    ///
    /// Calls `World::tick_async`, then `World::tick_sync`, then
    /// `World::tick_lazy`.
    ///
    /// ## Panics
    /// Panics if any sub-tick step returns an error
    pub fn tick(&mut self) {
        self.tick_async().unwrap();
        self.tick_sync().unwrap();
        self.tick_lazy().unwrap();
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
    ///
    /// Also runs component entity and archetype upkeep.
    pub fn tick_lazy(&mut self) -> anyhow::Result<()> {
        log::trace!("tick lazy");

        while let Ok(LazyOp { op, mut tx }) = self.lazy_ops.1.try_recv() {
            self.resource_manager.unify_resources("World::tick_lazy")?;
            let t = (op)(self)?;
            let _ = tx.send(t);
        }

        self.resource_manager.unify_resources("World::tick_lazy")?;
        let dead_ids: Vec<usize> = {
            let entities: &Entities = self.resource()?;
            let mut dead_ids = vec![];
            while let Some(id) = entities.delete_rx.try_recv().ok() {
                dead_ids.push(id);
            }
            dead_ids
        };
        if !dead_ids.is_empty() {
            let ids_and_types: Vec<(usize, smallvec::SmallVec<[TypeId; 4]>)> = {
                let archset: &mut Components = self.resource_mut()?;
                let ids_and_types = archset.upkeep(&dead_ids);
                ids_and_types
            };
            let entities: &mut Entities = self.resource_mut()?;
            entities
                .recycle
                .extend(ids_and_types.iter().map(|(id, _)| *id));
            entities
                .deleted
                .push_front((crate::system::current_iteration(), ids_and_types));
            // listeners have 3 frames to check for deleted things
            while entities.deleted.len() > 3 {
                let _ = entities.deleted.pop_back();
            }
        }

        Ok(())
    }

    /// Run all systems and futures until they have finished or one system has
    /// erred, whichever comes first.
    pub fn run(&mut self) -> &mut Self {
        loop {
            self.tick();
            if self.async_systems.is_empty()
                && self.sync_schedule.is_empty()
                && self.async_task_executor.is_empty()
            {
                break;
            }
        }

        self
    }

    /// Attempt to get a reference to one resource.
    pub fn resource<T: IsResource>(&self) -> anyhow::Result<&T> {
        let id = ResourceId::new::<T>();
        self.resource_manager.get(&id)
    }

    /// Attempt to get a mutable reference to one resource.
    pub fn resource_mut<T: IsResource>(&mut self) -> anyhow::Result<&mut T> {
        self.resource_manager.get_mut::<T>()
    }

    pub fn fetch<T: CanFetch>(&mut self) -> anyhow::Result<T> {
        self.resource_manager.unify_resources("World::fetch")?;
        T::construct(&mut LoanManager(&mut self.resource_manager))
    }

    /// Create a new entity.
    pub fn entity(&mut self) -> Entity {
        let mut entities = self.fetch::<Write<Entities>>().unwrap();
        entities.create()
    }

    /// Create a new entity and immediately attach the bundle of components to
    /// it.
    pub fn entity_with_bundle<B: IsBundle>(&mut self, bundle: B) -> Entity {
        let entity = self.resource_mut::<Entities>().unwrap().create();
        self.resource_mut::<Components>()
            .unwrap()
            .insert_bundle(entity.id(), bundle);
        entity
    }

    pub fn query<Q: IsQuery + 'static>(&mut self) -> QueryGuard<'_, Q> {
        self.resource_mut::<Components>().unwrap().query::<Q>()
    }

    pub fn get_schedule_description(&self) -> String {
        format!("{:#?}", self.sync_schedule)
    }

    /// Returns the scheduled systems' names, collated by batch.
    pub fn get_sync_schedule_names(&self) -> Vec<Vec<&str>> {
        self.sync_schedule.get_schedule_names()
    }
}

#[cfg(test)]
mod test {
    use std::sync::Mutex;

    use crate::{
        self as apecs, anyhow,
        chan::spsc,
        storage::{Components, Query},
        system::*,
        world::*,
        CanFetch, Read, Write,
    };
    use rustc_hash::FxHashMap;

    #[derive(Default)]
    struct MyMap(FxHashMap<String, u32>);

    #[derive(Default)]
    struct Number(u32);

    #[test]
    fn can_closure_system() {
        #[derive(Copy, Clone, Debug, PartialEq)]
        struct F32s(f32, f32);

        #[derive(CanFetch)]
        struct StatefulSystemData {
            positions: Query<&'static F32s>,
        }

        fn mk_stateful_system(
            tx: spsc::Sender<F32s>,
        ) -> impl FnMut(StatefulSystemData) -> anyhow::Result<ShouldContinue> {
            println!("making stateful system");
            let mut highest_pos: F32s = F32s(0.0, f32::NEG_INFINITY);

            move |data: StatefulSystemData| {
                println!("running stateful system: highest_pos:{:?}", highest_pos);
                for pos in data.positions.query().iter_mut() {
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

        let (tx, rx) = spsc::bounded(1);

        let mut world = World::default();
        world
            .with_system("stateful", mk_stateful_system(tx))
            .unwrap();
        {
            let mut archset: Write<Components> = world.fetch().unwrap();
            archset.insert_component(0, F32s(20.0, 30.0));
            archset.insert_component(1, F32s(0.0, 0.0));
            archset.insert_component(2, F32s(100.0, 100.0));
        }

        world.tick();

        let highest = rx.try_recv().unwrap();
        assert_eq!(F32s(100.0, 100.0), highest);
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
            facade
                .visit(
                    |(mut entities, mut archset): (Write<Entities>, Write<Components>)| {
                        for n in 0..100u32 {
                            let e = entities.create();
                            archset.insert_bundle(e.id(), (format!("entity_{}", n), n));
                        }

                        Ok(())
                    },
                )
                .await
        }

        fn maintain_map(
            mut data: (Query<(&String, &u32)>, Write<MyMap>),
        ) -> anyhow::Result<ShouldContinue> {
            for (name, number) in data.0.query().iter_mut() {
                if !data.1.inner().0.contains_key(name.value()) {
                    let _ = data
                        .1
                        .inner_mut()
                        .0
                        .insert(name.to_string(), *number.value());
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
        world.tick();
        rx.try_recv().unwrap();

        // world sends resources to the create system which makes
        // entities+components, then the maintain system updates the book
        world.tick();

        let book = world.fetch::<Read<MyMap>>().unwrap();
        for n in 0..100 {
            assert_eq!(book.0.get(&format!("entity_{}", n)), Some(&n));
        }
    }

    #[test]
    fn can_create_entities_and_build_convenience() {
        struct DataA(f32);
        struct DataB(f32);

        let mut world = World::default();
        assert!(world.has_resource::<Entities>(), "missing entities");
        let e = world.entity().with_bundle((DataA(0.0), DataB(0.0)));
        let id = e.id();

        world.with_async(async move {
            println!("updating entity");
            e.with_bundle((DataA(666.0), DataB(666.0))).updates().await;
            println!("done!");
        });

        while !world.async_task_executor.is_empty() {
            world.tick();
        }

        let data: Query<(&DataA, &DataB)> = world.fetch().unwrap();
        let mut q = data.query();
        let (a, b) = q.find_one(id).unwrap();
        assert_eq!(666.0, a.0);
        assert_eq!(666.0, b.0);
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
                let e = facade
                    .visit(|mut entities: Write<Entities>| Ok(entities.create()))
                    .await
                    .unwrap();
                await_points.lock().unwrap().push(2);
                e
            };

            e.insert_bundle((Name("ada"), Age(666)));
            e.updates().await;
            await_points.lock().unwrap().push(3);

            let (name, age) = e
                .visit::<(&Name, &Age), _>(|(name, age)| {
                    (name.value().clone(), age.value().clone())
                })
                .await
                .unwrap();
            await_points.lock().unwrap().push(4);
            assert_eq!(Name("ada"), name);
            assert_eq!(Age(666), age);

            println!("done!");
            Ok(())
        });

        for i in 1..=5 {
            world.tick();
            // there should only be 4 awaits because the system exits after the 4th
            let num_awaits = i.min(4);
            assert_eq!(Some(num_awaits), awaits.lock().unwrap().last().cloned());
        }

        let ages = world.fetch::<Query<&Age>>().unwrap();
        let mut q = ages.query();
        let age = q.find_one(0).unwrap();
        assert_eq!(&Age(666), age.value());
    }

    #[test]
    fn plugin_inserts_resources_from_canfetch_in_systems() {
        #[derive(Default)]
        struct MyStr(&'static str);

        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world.with_system("test", |_: Write<MyStr>| ok()).unwrap();
        let s = world.resource_mut::<MyStr>().unwrap();
        s.0 = "blah";
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
            .with_system("one", |q: Query<(&f32, &bool)>| {
                for (_f, _b) in q.query().iter_mut() {}
                ok()
            })
            .unwrap()
            .with_system("two", |q: Query<(&f32, &bool)>| {
                for (_f, _b) in q.query().iter_mut() {}
                ok()
            })
            .unwrap();
        world.tick();
    }

    #[test]
    fn parallelism() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world
            .with_async_system("one", |mut facade: Facade| -> AsyncSystemFuture {
                Box::pin(async move {
                    facade
                        .visit(|mut number: Write<Number>| {
                            number.inner_mut().0 = 1;
                            Ok(())
                        })
                        .await
                })
            })
            .with_async_system("two", |mut facade: Facade| -> AsyncSystemFuture {
                Box::pin(async move {
                    for _ in 0..2 {
                        facade
                            .visit(|mut number: Write<Number>| {
                                number.inner_mut().0 = 2;
                                Ok(())
                            })
                            .await?;
                    }
                    Ok(())
                })
            })
            .with_async_system("three", |mut facade: Facade| -> AsyncSystemFuture {
                Box::pin(async move {
                    for _ in 0..3 {
                        facade
                            .visit(|mut number: Write<Number>| {
                                number.inner_mut().0 = 3;
                                Ok(())
                            })
                            .await?;
                    }
                    Ok(())
                })
            })
            .with_parallelism(Parallelism::Automatic);
        world.run();
    }

    #[test]
    fn deleted_entities() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        {
            let entities: &mut Entities = world.resource_mut().unwrap();
            (0..10u32).for_each(|i| {
                entities
                    .create()
                    .insert_bundle(("hello".to_string(), false, i))
            });
            assert_eq!(10, entities.alive_len());
        }
        world.tick();
        {
            let q: Query<&u32> = world.fetch().unwrap();
            assert_eq!(9, **q.query().find_one(9).unwrap());
        }
        {
            let entities: &Entities = world.resource().unwrap();
            let entity = entities.hydrate(9).unwrap();
            entities.destroy(entity);
        }
        world.tick();
        {
            let entities: &Entities = world.resource().unwrap();
            assert_eq!(9, entities.alive_len());
            let deleted_strings = entities.deleted_iter_of::<String>().collect::<Vec<_>>();
            assert_eq!(9, deleted_strings[0].id());
        }
    }
}
