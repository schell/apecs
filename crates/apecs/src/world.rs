//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::{
    any::{Any, TypeId},
    collections::VecDeque,
    ops::Deref,
    sync::atomic::AtomicU64,
    sync::Arc,
};

use moongraph::{Edges, Graph, GraphError, ViewMut};

use crate::{
    //plugin::Plugin,
    facade::{Facade, FacadeSchedule, Request},
    storage::{Components, Entry, IsBundle, IsQuery},
};

static SYSTEM_ITERATION: AtomicU64 = AtomicU64::new(0);

#[inline]
/// Get the current system iteration timestamp.
///
/// This can be used to track changes in components over time with
/// [`Entry::has_changed_since`](crate::Entry::has_changed_since) and similar
/// functions.
pub fn current_iteration() -> u64 {
    SYSTEM_ITERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Increment the system iteration counter, returning the previous value.
#[inline]
fn increment_current_iteration() -> u64 {
    SYSTEM_ITERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A lazy mutation of the `World` and its result.
pub struct LazyOp {
    op: Box<
        dyn FnOnce(&mut World) -> Result<Arc<dyn Any + Send + Sync>, GraphError>
            + Send
            + Sync
            + 'static,
    >,
    tx: async_channel::Sender<Arc<dyn Any + Send + Sync>>,
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
    op_sender: async_channel::Sender<LazyOp>,
    op_receivers: Vec<async_channel::Receiver<Arc<dyn Any + Send + Sync>>>,
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

impl AsRef<usize> for Entity {
    fn as_ref(&self) -> &usize {
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
        let (tx, rx) = async_channel::bounded(1);
        let op = LazyOp {
            op: Box::new(move |world: &mut World| {
                let all = world.get_components_mut();
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
        let (tx, rx) = async_channel::bounded(1);
        let op = LazyOp {
            op: Box::new(move |world: &mut World| {
                let all = world.get_components_mut();
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
        let (tx, rx) = async_channel::bounded(1);
        let op = LazyOp {
            op: Box::new(move |world: &mut World| {
                let all = world.get_components_mut();
                let _ = all.remove_component::<T>(id);
                Ok(Arc::new(()) as Arc<dyn Any + Send + Sync>)
            }),
            tx,
        };
        // UNWRAP: safe because this channel is unbounded
        self.op_sender.try_send(op).unwrap();
        self.op_receivers.push(rx);
    }

    /// Await a future that completes after all lazy updates have been
    /// performed.
    pub async fn updates(&mut self) {
        let updates: Vec<async_channel::Receiver<Arc<dyn Any + Send + Sync>>> =
            std::mem::take(&mut self.op_receivers);
        for update in updates.into_iter() {
            // TODO: explain why this unwrap is safe...
            let _ = update.recv().await.unwrap();
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
        let (tx, rx) = async_channel::bounded(1);
        // UNWRAP: safe because this channel is unbounded
        self.op_sender
            .try_send(LazyOp {
                op: Box::new(move |world: &mut World| -> Result<_, GraphError> {
                    let storage = world.get_components_mut();
                    let mut q = storage.query::<Q>();
                    Ok(Arc::new(q.find_one(id).map(f)) as Arc<dyn Any + Send + Sync>)
                }),
                tx,
            })
            .unwrap();
        let arc: Arc<dyn Any + Send + Sync> = rx
            .recv()
            .await
            .map_err(|_| log::error!("could not receive get request"))
            .unwrap();
        let arc_c: Arc<Option<T>> = arc
            .downcast()
            .map_err(|_| log::error!("could not downcast '{}'", std::any::type_name::<T>()))
            .unwrap();
        let c: Option<T> = Arc::try_unwrap(arc_c)
            .map_err(|_| log::error!("could not unwrap '{}'", std::any::type_name::<T>()))
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
    // unbounded
    pub(crate) delete_tx: async_channel::Sender<usize>,
    // unbounded
    pub(crate) delete_rx: async_channel::Receiver<usize>,
    pub(crate) deleted: VecDeque<(u64, Vec<(usize, smallvec::SmallVec<[TypeId; 4]>)>)>,
    // unbounded
    pub(crate) lazy_op_sender: async_channel::Sender<LazyOp>,
}

impl Default for Entities {
    fn default() -> Self {
        let (delete_tx, delete_rx) = async_channel::unbounded();
        Self {
            next_k: Default::default(),
            generations: vec![],
            recycle: Default::default(),
            delete_rx,
            delete_tx,
            deleted: Default::default(),
            lazy_op_sender: async_channel::unbounded().0,
        }
    }
}

impl Entities {
    fn new(lazy_op_sender: async_channel::Sender<LazyOp>) -> Self {
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

/// Defines the number of threads to use for inner and outer parallelism.
pub enum Parallelism {
    Automatic,
    Explicit(u32),
}

// Only used internally and purposefully kept **out** of the graph, so that
// it won't hang up `World::run_loop`, which depends on checking to see if
// there are any systems in the graph (if not, it can exit).
#[derive(Edges)]
struct EntityUpkeepSystem {
    entities: ViewMut<Entities>,
    components: ViewMut<Components>,
}

impl EntityUpkeepSystem {
    fn tick(mut self) -> Result<(), GraphError> {
        let dead_ids: Vec<usize> = {
            let mut dead_ids = vec![];
            while let Some(id) = self.entities.delete_rx.try_recv().ok() {
                dead_ids.push(id);
            }
            dead_ids
        };
        if !dead_ids.is_empty() {
            let ids_and_types: Vec<(usize, smallvec::SmallVec<[TypeId; 4]>)> = {
                let ids_and_types = self.components.upkeep(&dead_ids);
                ids_and_types
            };
            self.entities
                .recycle
                .extend(ids_and_types.iter().map(|(id, _)| *id));
            self.entities
                .deleted
                .push_front((crate::world::current_iteration(), ids_and_types));
            // listeners have 3 frames to check for deleted things
            while self.entities.deleted.len() > 3 {
                let _ = self.entities.deleted.pop_back();
            }
        }

        Ok(())
    }
}

/// A collection of resources and systems.
///
/// The `World` holds all resources, entities, components and systems.
///
/// Most applications will create and configure a `World` in their main
/// function and call [`World::run_loop`] or [`World::tick`] or [`World::tock`]
/// to run all systems.
///
/// How to run async futures is up to you, but those futures can interact with
/// with the [`World`] through a [`Facade`].
///
/// ```rust
/// use apecs::*;
/// let executor = std::sync::Arc::new(async_executor::Executor::new());
/// let _execution_loop = {
///     let executor = executor.clone();
///     std::thread::spawn(move || loop {
///         match std::sync::Arc::strong_count(&executor) {
///             1 => break,
///             _ => {
///                 let _ = executor.try_tick();
///             }
///         }
///     })
/// };
/// struct Channel<T> {
///     tx: async_broadcast::Sender<T>,
///     rx: async_broadcast::Receiver<T>,
/// }
/// impl<T> Default for Channel<T> {
///     fn default() -> Self {
///         let (tx, rx) = async_broadcast::broadcast(3);
///         Channel { tx, rx }
///     }
/// }
/// /// Anything that derives `Edges` can be used as system parameters
/// /// and visited by a `Facade`.
/// ///
/// /// Its fields will be stored in the `World` as individual resources.
/// #[derive(Edges)]
/// struct MyAppData {
///     channel: View<Channel<String>>,
///     count: ViewMut<usize>,
/// }
/// /// `compute_hello` is a system that visits `MyAppData`, accumlates a count and
/// /// sends a message into a channel the third time it is run.
/// fn compute_hello(mut data: MyAppData) -> Result<(), GraphError> {
///     if *data.count >= 3 {
///         data.channel
///             .tx
///             .try_broadcast("hello world".to_string())
///             .unwrap();
///         end()
///     } else {
///         *data.count += 1;
///         ok()
///     }
/// }
/// let mut world = World::default();
/// world.add_subgraph(graph!(compute_hello));
/// // Create a facade to move into an async future that awaits a message from the
/// // `compute_hello` system.
/// let mut facade = world.facade();
/// executor
///     .spawn(async move {
///         // Visit the world's `MyAppData` to get the receiver.
///         let mut rx = facade
///             .visit(|data: MyAppData| data.channel.rx.clone())
///             .await
///             .unwrap();
///         if let Ok(msg) = rx.recv().await {
///             println!("got message: {}", msg);
///         }
///     })
///     .detach();
/// while !executor.is_empty() {
///     // `tick` progresses the world by one frame
///     world.tick().unwrap();
///     // send out any requests the facade may have made from a future running in our
///     // executor
///     let mut facade_schedule = world.take_facade_schedule().unwrap();
///     facade_schedule.run().unwrap();
/// }
/// ```
///
/// `World` is the outermost object of `apecs`, as it contains all systems and
/// resources. It contains [`Entities`] and [`Components`] as default resources.
/// You can create entities, attach components and query them from outside the
/// world if desired:
///
/// ```rust
/// use apecs::*;
/// let mut world = World::default();
/// let entities = world.get_entities_mut();
/// // Nearly any type can be used as a component with zero boilerplate
/// let a = entities.create().with_bundle((123, true, "abc"));
/// let b = entities.create().with_bundle((42, false));
///
/// // Query the world for all matching bundles
/// let mut query = world.get_components_mut().query::<(&mut i32, &bool)>();
/// for (number, flag) in query.iter_mut() {
///     if **flag {
///         **number *= 2;
///     }
/// }
///
/// // Perform random access within the same query by using the entity.
/// let b_i32 = **query.find_one(b.id()).unwrap().0;
/// assert_eq!(b_i32, 42);
///
/// // Track changes to individual components
/// let a_entry: &Entry<i32> = query.find_one(a.id()).unwrap().0;
/// assert_eq!(**a_entry, 246);
/// assert_eq!(apecs::current_iteration(), a_entry.last_changed());
/// ```
///
/// ## Where to look next 📚
/// * [`Entities`] for info on creating and deleting [`Entity`]s
/// * [`Components`] for info on creating, updating and deleting components
/// * [`Entry`] for info on tracking changes to individual components
/// * [`Query`](crate::Query) for info on querying bundles of components
/// * [`Facade`] for info on interacting with the `World` from a future
///   operations together into an easy-to-integrate package
pub struct World {
    pub(crate) graph: Graph,
    pub(crate) facade: Facade,
    pub(crate) facade_requests: async_channel::Receiver<Request>,
    pub(crate) facade_graph: Graph,
    pub(crate) lazy_ops: (
        async_channel::Sender<LazyOp>,
        async_channel::Receiver<LazyOp>,
    ),
}

impl Default for World {
    fn default() -> Self {
        let lazy_ops = async_channel::unbounded();
        let entities = Entities::new(lazy_ops.0.clone());
        let (tx, rx) = async_channel::unbounded();
        let facade = Facade { request_tx: tx };
        let mut world = Self {
            graph: Graph::default(),
            facade: facade.clone(),
            facade_requests: rx,
            facade_graph: Graph::default(),
            lazy_ops,
        };

        world
            .add_resource(entities)
            .add_resource(Components::default())
            .add_resource(facade);

        world
    }
}

impl World {
    // /// Create a `Plugin` to build the world.
    // pub fn builder() -> Plugin {
    //     Plugin::default()
    // }

    /// Create a new facade
    pub fn facade(&self) -> Facade {
        self.facade.clone()
    }

    /// Returns the total number of [`Facade`]s.
    pub fn facade_count(&self) -> usize {
        self.facade.count()
    }

    pub fn add_subgraph(&mut self, graph: Graph) -> &mut Self {
        self.graph.add_subgraph(graph);
        let _ = self.graph.reschedule_if_necessary();
        self
    }

    pub fn interleave_subgraph(&mut self, graph: Graph) -> &mut Self {
        self.graph.interleave_subgraph(graph);
        let _ = self.graph.reschedule_if_necessary();
        self
    }

    pub fn contains_resource<T: Any + Send + Sync>(&self) -> bool {
        self.graph.contains_resource::<T>()
    }

    pub fn add_resource<T: Any + Send + Sync>(&mut self, t: T) -> &mut Self {
        self.graph.add_resource(t);
        self
    }

    pub fn get_resource<T: Any + Send + Sync>(&self) -> Option<&T> {
        // UNWRAP: if T cannot be downcast we want to panic
        self.graph.get_resource::<T>().unwrap()
    }

    pub fn get_resource_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
        // UNWRAP: if T cannot be downcast we want to panic
        self.graph.get_resource_mut::<T>().unwrap()
    }

    /// Visit world resources with a closure.
    ///
    /// This is like running a one-off system, but `S` does not get packed
    /// into the world as a result resource, instead it is given back to the
    /// callsite.
    ///
    /// ## Note
    /// By design, visiting the world with a type that uses `Move` in one of its
    /// fields will result in the wrapped type of that field being `move`d
    /// **out** of the world. The resource will no longer be available
    /// within the world.
    ///
    /// ```rust
    /// use apecs::*;
    /// use snafu::prelude::*;
    ///
    /// #[derive(Debug, Snafu)]
    /// enum TestError {}
    ///
    /// #[derive(Edges)]
    /// struct Input {
    ///     num_usize: View<usize>,
    ///     num_f32: ViewMut<f32>,
    ///     num_f64: Move<f64>,
    /// }
    ///
    /// // pack the graph with resources
    /// let mut graph = Graph::default()
    ///     .with_resource(0usize)
    ///     .with_resource(0.0f32)
    ///     .with_resource(0.0f64);
    ///
    /// // visit the graph, reading, modifying and _moving_!
    /// let num_usize = graph.visit(|mut input: Input| {
    ///     *input.num_f32 = 666.0;
    ///     *input.num_f64 += 10.0;
    ///     *input.num_usize
    /// }).unwrap();
    ///
    /// // observe we read usize
    /// assert_eq!(0, num_usize);
    /// assert_eq!(0, *graph.get_resource::<usize>().unwrap().unwrap());
    ///
    /// // observe we modified f32
    /// assert_eq!(666.0, *graph.get_resource::<f32>().unwrap().unwrap());
    ///
    /// // observe we moved f64 out of the graph and it is no longer present
    /// assert!(!graph.contains_resource::<f64>());
    pub fn visit<T: Edges, S>(&mut self, f: impl FnOnce(T) -> S) -> Result<S, GraphError> {
        self.graph.visit(f)
    }

    // /// Add a plugin to the world, instantiating any missing resources or
    // /// systems.
    // ///
    // /// ## Errs
    // /// Errs if the plugin requires resources that cannot be created by default.
    // pub fn with_plugin(&mut self, plugin: impl Into<Plugin>) -> anyhow::Result<&mut Self> {
    //     let plugin: Plugin = plugin.into();

    //     let mut missing_resources: FxHashMap<ResourceId, Vec<anyhow::Error>> = FxHashMap::default();
    //     for LazyResource { id, create } in plugin.resources.into_iter() {
    //         if !self.resource_manager.has_resource(&id) {
    //             log::debug!("attempting to create resource {}...", id.name);
    //             match (create)(&mut self.resource_manager.as_mut_loan_manager()) {
    //                 Ok(resource) => {
    //                     missing_resources.remove(&id);
    //                     let _ = self.resource_manager.insert(id, resource);
    //                 }
    //                 Err(err) => {
    //                     let entry = missing_resources.entry(id).or_default();
    //                     entry.push(err);
    //                 }
    //             }
    //             self.resource_manager
    //                 .unify_resources("after building lazy dep")?;
    //         }
    //     }

    //     anyhow::ensure!(
    //         missing_resources.is_empty(),
    //         "missing resources:\n{:#?}",
    //         missing_resources
    //     );

    //     for system in plugin.sync_systems.into_iter() {
    //         if !self.system_schedule.contains_system(system.0.name()) {
    //             self.system_schedule.add_system(system.0);
    //         }
    //     }

    //     Ok(self)
    // }

    // pub fn with_data<T: CanFetch>(&mut self) -> anyhow::Result<&mut Self> {
    //     self.with_plugin(T::plugin())
    // }

    pub fn with_parallelism(&mut self, parallelism: Parallelism) -> &mut Self {
        match parallelism {
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
        self
    }

    /// Conduct a world tick.
    pub fn tick(&mut self) -> Result<(), GraphError> {
        self.visit(EntityUpkeepSystem::tick)??;

        self.tick_sync()?;
        self.tick_lazy()?;

        Ok(())
    }

    /// Conduct a world tick, but panic if the result is not `Ok`.
    ///
    /// ## Panics
    /// Panics if the result is not `Ok`
    pub fn tock(&mut self) {
        self.tick().unwrap()
    }

    /// Just tick the synchronous systems.
    pub fn tick_sync(&mut self) -> Result<(), GraphError> {
        log::trace!("tick sync");
        // This is basically `moongraph::run_with_local` but with our own
        // system::increment_system_counter interleaved so we can track component
        // changes.
        self.graph.reschedule_if_necessary()?;

        let mut local: Option<fn(_) -> Result<_, _>> = None;
        let mut got_trimmed = false;
        let mut batches = self.graph.batches();
        while let Some(batch) = batches.next_batch() {
            let batch_result = batch.run(&mut local)?;
            let did_trim_batch = batch_result.save(true)?;
            got_trimmed = got_trimmed || did_trim_batch;
            let _ = increment_current_iteration();
        }
        if got_trimmed {
            self.graph.reschedule()?;
        }

        Ok(())
    }

    /// Applies lazy world updates and runs component entity / archetype upkeep.
    pub fn tick_lazy(&mut self) -> Result<(), GraphError> {
        log::trace!("tick lazy");

        while let Ok(LazyOp { op, tx }) = self.lazy_ops.1.try_recv() {
            let t = (op)(self)?;
            let _ = tx.try_send(t);
        }

        Ok(())
    }

    /// Return whether or not there are any requests for world resources from one
    /// or more [`Facade`]s.
    pub fn has_facade_requests(&self) -> bool {
        !self.facade_requests.is_empty()
    }

    pub fn take_facade_schedule(&mut self) -> Result<FacadeSchedule, GraphError> {
        self.facade_graph = Graph::default();

        // fetch all the requests for system resources and fold them into a schedule
        let mut i = 0;
        while let Ok(request) = self.facade_requests.try_recv() {
            let node = moongraph::Node::from(request).with_name(format!("request-{}", i));
            self.facade_graph.add_node(node);
            i += 1;
        }
        self.facade_graph.reschedule()?;

        let mut batches = self.facade_graph.batches();
        batches.set_resources(self.graph._resources_mut());
        Ok(FacadeSchedule { batches })
    }

    /// Run all systems until one of the following conditions have been met:
    /// * All systems have finished successfully
    /// * One system has erred
    /// * One or more requests have been received from a [`Facade`]
    ///
    /// ## Note
    /// This does not send out resources to the [`Facade`]. For async support it
    /// is recommended you use [`World::run`] to progress the world's
    /// systems, followed by [`World::take_facade_schedule`] and [`RequestSchedule::tick`].
    ///
    /// ```rust
    /// use apecs::{World, FacadeSchedule};
    ///
    /// let mut world = World::default();
    /// //...populate the world
    ///
    /// // run systems until the facade makes a request for resources
    /// world.run_loop().unwrap();
    ///
    /// // answer requests for world resources from external futures
    /// // until all requests are met
    /// let mut facade_schedule = world.take_facade_schedule().unwrap();
    /// facade_schedule.run().unwrap();
    /// ```
    pub fn run_loop(&mut self) -> Result<&mut Self, GraphError> {
        loop {
            self.tick()?;
            if self.has_facade_requests() || self.graph.node_len() == 0 {
                break;
            }
        }

        Ok(self)
    }

    /// Return a reference to [`Components`].
    pub fn get_components(&self) -> &Components {
        // UNWRAP: safe because we always have Components
        self.get_resource::<Components>().unwrap()
    }

    /// Return a mutable reference to [`Components`].
    pub fn get_components_mut(&mut self) -> &mut Components {
        // UNWRAP: safe because we always have Components
        self.get_resource_mut::<Components>().unwrap()
    }

    /// Return a reference to [`Entities`].
    pub fn get_entities(&self) -> &Entities {
        // UNWRAP: safe because we always have Entities
        self.get_resource::<Entities>().unwrap()
    }

    /// Return a mutable reference to [`Entities`].
    pub fn get_entities_mut(&mut self) -> &mut Entities {
        // UNWRAP: safe because we always have Entities
        self.get_resource_mut::<Entities>().unwrap()
    }

    /// Returns the scheduled systems' names, collated by batch.
    pub fn get_schedule_names(&mut self) -> Vec<Vec<&str>> {
        self.graph.get_schedule()
    }
}
