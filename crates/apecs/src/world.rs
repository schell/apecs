//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::any::TypeId;
use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;
use std::{any::Any, collections::VecDeque};

use anyhow::Context;
use async_executor::{Executor, Task};
use rustc_hash::FxHashMap;

use crate::QueryGuard;
use crate::{
    chan::{self, mpsc, oneshot, spsc},
    plugin::Plugin,
    resource_manager::{LoanManager, ResourceManager},
    schedule::{Dependency, IsSchedule},
    storage::{Components, Entry, IsBundle, IsQuery},
    system::{AsyncSchedule, ShouldContinue, SyncSchedule, SyncSystem},
    CanFetch, IsResource, LazyResource, Request, Resource, ResourceId, Write,
};

/// Visits world resources from async systems.
///
/// A facade is a window into the world, by which an async system can interact
/// with the world through [`Facade::visit`], without causing resource
/// contention.
#[derive(Clone)]
pub struct Facade {
    // Unbounded. Sending a request from the system should not yield the async
    pub(crate) resource_request_tx: mpsc::Sender<Request>,
    pub(crate) executor: Arc<async_executor::Executor<'static>>,
}

impl Facade {
    /// Asyncronously visit fetchable system resources using a closure.
    ///
    /// The closure may return data to the caller.
    ///
    /// A roundtrip takes at most one frame.
    ///
    /// **Note**: Using a closure ensures that no fetched system resources are
    /// held over an await point, which would preclude other systems from
    /// accessing them and susequently being able to run.
    pub async fn visit<D: CanFetch + Send + Sync + 'static, T: Send + Sync + 'static>(
        &mut self,
        f: impl FnOnce(D) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let borrows = D::borrows();
        // request the resources from the world
        // these requests are gathered in World::tick_async into an AsyncSchedule
        // and then run with the executor and resource maanger
        let (deploy_tx, deploy_rx) = spsc::bounded(1);
        // UNWRAP: safe because the request channel is unbounded
        self.resource_request_tx
            .try_send(Request {
                borrows,
                construct: |loan_mngr: &mut LoanManager| {
                    let my_d = D::construct(loan_mngr).with_context(|| {
                        format!("could not construct {}", std::any::type_name::<D>())
                    })?;
                    let my_d_in_a_box: Box<D> = Box::new(my_d);
                    let rez = Resource::from(my_d_in_a_box);
                    Ok(rez)
                },
                deploy_tx,
            })
            .unwrap();
        let rez: Resource = deploy_rx.recv().await.unwrap();
        let box_d: Box<D> = rez.downcast().map_err(|rez| {
            anyhow::anyhow!(
                "Facade could not downcast resource '{}' to '{}'",
                rez.type_name().unwrap_or("unknown"),
                std::any::type_name::<D>()
            )
        })?;
        let d = *box_d;
        let t = f(d)?;
        Ok(t)
    }

    /// Spawn a task onto the world's async task executor.
    pub fn spawn<T: Send + Sync + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        self.executor.spawn(future)
    }
}

/// A lazy mutation of the `World` and its result.
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
/// world
///     .with_async("spawn-b-entity", |mut facade: Facade| async move {
///         let mut b = facade
///             .visit(|mut ents: Write<Entities>| {
///                 Ok(ents.create().with_bundle((123, "entity B", true)))
///             })
///             .await?;
///         // after this await `b` has its bundle
///         b.updates().await;
///         // visit a component or bundle
///         let did_visit = b
///             .visit::<&&str, ()>(|name| assert_eq!("entity B", *name.value()))
///             .await
///             .is_some();
///         Ok(())
///     })
///     .unwrap();
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
        let (tx, rx) = oneshot::oneshot();
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
        let (tx, rx) = oneshot::oneshot();
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
        let (tx, rx) = oneshot::oneshot();
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
        let (tx, rx) = oneshot::oneshot();
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

    /// Return an iterator over all alive entities.
    pub fn alive_iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.generations
            .iter()
            .enumerate()
            .filter_map(|(id, _gen)| self.hydrate(id))
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

    /// Destroys all entities.
    pub fn destroy_all(&mut self) {
        for id in 0..self.next_k {
            if let Some(entity) = self.hydrate(id) {
                self.destroy(entity);
            }
        }
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
    ///
    /// Returns `None` the entity with the given id does not exist, or has
    /// been destroyed.
    pub fn hydrate(&self, entity_id: usize) -> Option<Entity> {
        if self.recycle.contains(&entity_id) {
            return None;
        }
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

// pub(crate) fn make_async_system_pack<F, Fut>(
//    name: String,
//    async_task_executor: Arc<async_executor::Executor<'static>>,
//    make_system_future: F,
//) -> (AsyncSystem, AsyncSystemFuture)
// where
//    F: FnOnce(Facade) -> Fut,
//    Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
//{
//    let (resource_request_tx, resource_request_rx) = mpsc::unbounded();
//    let facade = Facade {
//        resource_request_tx,
//        async_task_executor,
//    };
//    let system = AsyncSystem {
//        name: name.clone(),
//        resource_request_rx,
//    };
//    let asys = Box::pin((make_system_future)(facade));
//    (system, asys)
//}

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
///     .with_async("await_hello", |mut facade: Facade| async move {
///         let mut rx = facade
///             .visit(|mut data: MyAppData| Ok(data.channel.new_receiver()))
///             .await?;
///         if let Ok(msg) = rx.recv().await {
///             println!("got message: {}", msg);
///         }
///         Ok(())
///     })
///     .unwrap();
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
    pub(crate) async_systems: FxHashMap<String, oneshot::Receiver<anyhow::Result<()>>>,
    pub(crate) facade: Facade,
    // TODO: change this to a "command" receiver in which `Request` is just one variant
    pub(crate) command_rx: mpsc::Receiver<Request>,
    pub(crate) lazy_ops: (mpsc::Sender<LazyOp>, mpsc::Receiver<LazyOp>),
}

impl Default for World {
    fn default() -> Self {
        let lazy_ops = mpsc::unbounded();
        let entities = Entities::new(lazy_ops.0.clone());
        let async_task_executor = Arc::new(Executor::default());
        let (tx, rx) = mpsc::unbounded();
        let facade = Facade {
            resource_request_tx: tx,
            executor: async_task_executor,
        };
        let mut world = Self {
            resource_manager: ResourceManager::default(),
            sync_schedule: SyncSchedule::default(),
            async_systems: FxHashMap::default(),
            facade: facade.clone(),
            command_rx: rx,
            lazy_ops,
        };

        world
            .with_resource(entities)
            .unwrap()
            .with_resource(Components::default())
            .unwrap()
            .with_resource(facade)
            .unwrap();
        world
    }
}

impl World {
    /// Create a `Plugin` to build the world.
    pub fn builder() -> Plugin {
        Plugin::default()
    }

    /// Create a new facade
    pub fn facade(&self) -> Facade {
        self.facade.clone()
    }

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
        let plugin: Plugin = plugin.into();

        let mut missing_resources: FxHashMap<ResourceId, Vec<anyhow::Error>> = FxHashMap::default();
        for LazyResource { id, create } in plugin.resources.into_iter() {
            if !self.resource_manager.has_resource(&id) {
                log::debug!("attempting to create resource {}...", id.name);
                match (create)(&mut self.resource_manager.as_mut_loan_manager()) {
                    Ok(resource) => {
                        missing_resources.remove(&id);
                        let _ = self.resource_manager.insert(id, resource);
                    }
                    Err(err) => {
                        let entry = missing_resources.entry(id).or_default();
                        entry.push(err);
                    }
                }
                self.resource_manager
                    .unify_resources("after building lazy dep")?;
            }
        }

        anyhow::ensure!(
            missing_resources.is_empty(),
            "missing resources:\n{:#?}",
            missing_resources
        );

        for system in plugin.sync_systems.into_iter() {
            if !self.sync_schedule.contains_system(&system.0.name) {
                self.sync_schedule.add_system(system.0);
            }
        }

        for asystem in plugin.async_systems.into_iter() {
            if self.async_systems.contains_key(&asystem.name) {
                continue;
            }

            self.with_async(asystem.name, asystem.make_future).unwrap();
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

    /// Spawn an asyncronous task that takes a `Facade` as input.
    pub fn with_async<F, Fut>(
        &mut self,
        name: impl Into<String>,
        make_system_future: F,
    ) -> anyhow::Result<&mut Self>
    where
        F: FnOnce(Facade) -> Fut,
        Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
    {
        let name = name.into();
        if self.async_systems.contains_key(&name) {
            anyhow::bail!("async system '{}' already exists", name);
        }

        let facade = self.facade.clone();
        let (mut tx, rx) = oneshot::oneshot();
        let fut = (make_system_future)(facade);
        let task = self.facade.spawn(async move {
            let result = fut.await;
            // UNWRAP: if we can't unwrap this all is lost
            tx.send(result).unwrap();
        });
        task.detach();
        self.async_systems.insert(name, rx);

        Ok(self)
    }

    /// Returns whether a resources of the given type exists in the world.
    pub fn has_resource<T: IsResource>(&self) -> bool {
        self.resource_manager.has_resource(&ResourceId::new::<T>())
    }

    /// Spawn an asynchronous task.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + Sync + 'static) {
        let task = self.facade.executor.spawn(future);
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
        Ok(())
    }

    /// Conduct a world tick, but panic if the result is not `Ok`.
    ///
    /// Calls `World::tick_async`, then `World::tick_sync`, then
    /// `World::tick_lazy`.
    ///
    /// ## Panics
    /// Panics if the result is not `Ok`
    pub fn tock(&mut self) {
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
        for (name, rx) in std::mem::take(&mut self.async_systems).into_iter() {
            // * if the channels are closed they have been dropped by the async system and
            //   we should no longer poll them for requests
            // * similarly, if they have finished we should no longer poll them
            match rx.try_recv() {
                Err(err) => match err {
                    oneshot::TryRecvError::Empty(rx) => {
                        self.async_systems.insert(name, rx);
                    }
                    oneshot::TryRecvError::Closed => {}
                },
                Ok(res) => res?,
            }
        }

        // fetch all the requests for system resources and fold them into a schedule
        let mut schedule = AsyncSchedule::default();
        while let Ok(request) = self.command_rx.try_recv() {
            schedule.add_system(request);
        }
        if !schedule.is_empty() {
            log::trace!(
                "async system execution:\n{}",
                schedule.get_execution_order().join("\n")
            );
            schedule.run(&self.facade.executor, &mut self.resource_manager)?;
        } else {
            fn tick(executor: &Executor<'static>) {
                while executor.try_tick() {}
            }
            // tick the executor
            let parallelism = self.sync_schedule.get_parallelism();
            if parallelism > 1 {
                rayon::prelude::ParallelIterator::for_each(
                    rayon::prelude::IntoParallelIterator::into_par_iter(0..parallelism as u32),
                    |_| tick(&self.facade.executor),
                );
            } else {
                tick(&self.facade.executor);
            }
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
    pub fn run(&mut self) -> anyhow::Result<&mut Self> {
        loop {
            self.tick()?;
            if self.async_systems.is_empty()
                && self.sync_schedule.is_empty()
                && self.facade.executor.is_empty()
            {
                break;
            }
        }

        Ok(self)
    }

    /// Run all systems and futures until the given async system ends, or the
    /// world encounters an error.
    pub fn run_while<T, F, Fut>(&mut self, make_system_future: F) -> anyhow::Result<T>
    where
        T: Send + Sync + 'static,
        F: FnOnce(Facade) -> Fut,
        Fut: Future<Output = T> + Send + Sync + 'static,
    {
        let (tx, rx) = spsc::bounded(1);
        let fut = (make_system_future)(self.facade.clone());
        self.spawn(async move {
            let t = fut.await;
            // UNWRAP: safe because we will not drop the receiver until after receiving
            tx.try_send(t).unwrap();
        });

        loop {
            self.tick()?;
            match rx.try_recv() {
                Ok(t) => return Ok(t),
                Err(err) => match err {
                    spsc::TryRecvError::Empty => {}
                    spsc::TryRecvError::Closed => unreachable!("this should never happen"),
                },
            }
        }
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

    pub fn insert_component<T: Send + Sync + 'static>(
        &mut self,
        id: usize,
        component: T,
    ) -> Option<T> {
        let components = self.resource_mut::<Components>().unwrap();
        components.insert_component(id, component)
    }

    pub fn insert_bundle<B: IsBundle>(&mut self, id: usize, bundle: B) {
        let components = self.resource_mut::<Components>().unwrap();
        components.insert_bundle(id, bundle);
    }

    pub fn get_component<T: Send + Sync + 'static>(
        &self,
        id: usize,
    ) -> Option<impl Deref<Target = T> + '_> {
        let components = self.resource::<Components>().unwrap();
        components.get_component::<T>(id)
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
    use futures_lite::future;
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

        world.tock();

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
            .with_async("create", |facade| async move { create(tx, facade).await })
            .unwrap()
            .with_system("maintain", maintain_map)
            .unwrap();

        // create system runs - sending on the channel and making the fetch request
        world.tock();
        rx.try_recv().unwrap();

        // world sends resources to the create system which makes
        // entities+components, then the maintain system updates the book
        world.tock();

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

        world
            .with_async("insert-bundle", |_| async move {
                println!("updating entity");
                e.with_bundle((DataA(666.0), DataB(666.0))).updates().await;
                println!("done!");
                Ok(())
            })
            .unwrap();

        while !world.facade.executor.is_empty() {
            world.tick().unwrap();
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
        world
            .with_async("test", |mut facade| async move {
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
            })
            .unwrap();

        for i in 1..=5 {
            world.tock();
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
        world.tock();
    }

    #[test]
    fn parallelism() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world
            .with_async("one", |mut facade: Facade| -> AsyncSystemFuture {
                Box::pin(async move {
                    facade
                        .visit(|mut number: Write<Number>| {
                            number.inner_mut().0 = 1;
                            Ok(())
                        })
                        .await
                })
            })
            .unwrap()
            .with_async("two", |mut facade: Facade| -> AsyncSystemFuture {
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
            .unwrap()
            .with_async("three", |mut facade: Facade| -> AsyncSystemFuture {
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
            .unwrap()
            .with_parallelism(Parallelism::Automatic);
        world.run().unwrap();
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
        world.tock();
        {
            let q: Query<&u32> = world.fetch().unwrap();
            assert_eq!(9, **q.query().find_one(9).unwrap());
        }
        {
            let entities: &Entities = world.resource().unwrap();
            let entity = entities.hydrate(9).unwrap();
            entities.destroy(entity);
        }
        world.tock();
        {
            let entities: &Entities = world.resource().unwrap();
            assert_eq!(9, entities.alive_len());
            let deleted_strings = entities.deleted_iter_of::<String>().collect::<Vec<_>>();
            assert_eq!(9, deleted_strings[0].id());
        }
    }

    #[test]
    fn spsc_drop_sanity() {
        // ensure the spsc channel delivers messages when the sender was dropped after
        // sending and before receiving
        let (tx, rx) = spsc::bounded::<()>(1);
        tx.try_send(()).unwrap();
        drop(tx);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn can_clone_facade() {
        let mut world = World::default();
        world
            .with_async("async", |mut facade_a: Facade| async move {
                let (tx, rx) = spsc::bounded(1);
                let mut facade_b = facade_a.clone();

                #[allow(unreachable_code)]
                let send_loop = async move {
                    loop {
                        println!("send_loop visiting");
                        let count = facade_a.visit(|count: Read<u32>| Ok(*count)).await?;
                        println!("send_loop sending");
                        tx.send(count).await.unwrap();
                    }

                    anyhow::Ok(())
                };

                #[allow(unreachable_code)]
                let recv_loop = async move {
                    loop {
                        println!("recv_loop receiving");
                        let recv_count: u32 = rx.recv().await.unwrap();
                        println!("recv_loop visiting");
                        facade_b
                            .visit(|mut count: Write<u32>| {
                                *count = recv_count + 1;
                                println!("recv_loop updated count to {}", recv_count + 1);
                                Ok(())
                            })
                            .await?;
                    }

                    anyhow::Ok(())
                };

                let _ = future::zip(send_loop, recv_loop).await;
                Ok(())
            })
            .unwrap()
            .run_while(|mut facade| async move {
                loop {
                    let count = facade.visit(|count: Read<u32>| Ok(*count)).await?;
                    println!("run_while loop counted {}", count);
                    if count >= 3 {
                        return anyhow::Ok(());
                    }
                }
            })
            .unwrap()
            .unwrap();
    }
}
