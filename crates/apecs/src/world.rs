//! The [`World`] contains resources and is responsible for ticking
//! systems.
use std::{
    any::{Any, TypeId},
    sync::atomic::AtomicU64,
    sync::Arc,
};

use moongraph::{Edges, Graph, GraphError, ViewMut};

use crate::{
    entity::Entities,
    facade::{Facade, FacadeSchedule, Request},
    storage::Components,
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
    pub(crate) op: Box<
        dyn FnOnce(&mut World) -> Result<Arc<dyn Any + Send + Sync>, GraphError>
            + Send
            + Sync
            + 'static,
    >,
    pub(crate) tx: async_channel::Sender<Arc<dyn Any + Send + Sync>>,
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
///
/// struct Channel<T> {
///     tx: async_broadcast::Sender<T>,
///     rx: async_broadcast::Receiver<T>,
/// }
///
/// impl<T> Default for Channel<T> {
///     fn default() -> Self {
///         let (tx, rx) = async_broadcast::broadcast(3);
///         Channel { tx, rx }
///     }
/// }
///
/// /// Anything that derives `Edges` can be used as system parameters
/// /// and visited by a `Facade`.
/// ///
/// /// Its fields will be stored in the `World` as individual resources.
/// #[derive(Edges)]
/// struct MyAppData {
///     channel: View<Channel<String>>,
///     count: ViewMut<usize>,
/// }
///
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
///
/// let mut world = World::default();
/// world.add_subgraph(graph!(compute_hello));
/// // Create a facade to move into an async future that awaits a message from the
/// // `compute_hello` system.
/// let mut facade = world.facade();
/// let task = smol::spawn(async move {
///     // Visit the world's `MyAppData` to get the receiver.
///     let mut rx = facade
///         .visit(|data: MyAppData| data.channel.rx.clone())
///         .await
///         .unwrap();
///     if let Ok(msg) = rx.recv().await {
///         println!("got message: {}", msg);
///     }
/// });
///
/// while !task.is_finished() {
///     // `tick` progresses the world by one frame
///     world.tick().unwrap();
///     // send out any requests the facade may have made from a future running in our
///     // executor
///     let mut facade_schedule = world.get_facade_schedule().unwrap();
///     facade_schedule.run().unwrap();
/// }
/// ```
///
/// `World` is the outermost object of `apecs`, as it contains all systems and
/// resources. It contains [`Entities`] and [`Components`] as default resources.
/// You can create entities, attach components and query them from outside the
/// world if desired:
///
///
/// ```rust
/// use apecs::*;
/// let mut world = World::default();
/// // Create entities to hold heterogenous components
/// let a = world.get_entities_mut().create();
/// let b = world.get_entities_mut().create();
/// // Nearly any type can be used as a component with zero boilerplate.
/// // Here we add three components as a "bundle" to entity "a".
/// world
///     .get_components_mut()
///     .insert_bundle(*a, (123i32, true, "abc"));
/// assert!(world
///     .get_components()
///     .get_component::<i32>(a.id())
///     .is_some());
/// // Add two components as a "bundle" to entity "b".
/// world.get_components_mut().insert_bundle(*b, (42i32, false));
/// // Query the world for all matching bundles
/// let mut query = world.get_components_mut().query::<(&mut i32, &bool)>();
/// for (number, flag) in query.iter_mut() {
///     println!("id: {}", number.id());
///     if **flag {
///         **number *= 2;
///     }
/// }
/// // Perform random access within the same query by using the entity.
/// let b_i32 = **query.find_one(b.id()).unwrap().0;
/// assert_eq!(b_i32, 42);
/// // Track changes to individual components
/// let a_entry: &Entry<i32> = query.find_one(a.id()).unwrap().0;
/// assert_eq!(apecs::current_iteration(), a_entry.last_changed());
/// assert_eq!(**a_entry, 246);
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
    pub(crate) fn tick_sync(&mut self) -> Result<(), GraphError> {
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
            let did_trim_batch = batch_result.save(true, true)?;
            got_trimmed = got_trimmed || did_trim_batch;
            let _ = increment_current_iteration();
        }
        if got_trimmed {
            self.graph.reschedule()?;
        }

        Ok(())
    }

    /// Applies lazy world updates and runs component entity / archetype upkeep.
    pub(crate) fn tick_lazy(&mut self) -> Result<(), GraphError> {
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
        self.facade_graph.node_len() > 0 || !self.facade_requests.is_empty()
    }

    /// Build the schedule of resource requests from the [`World`]'s [`Facade`]s.
    ///
    /// This schedule can be run with [`FacadeSchedule::run`] or ticked with
    /// [`FacadeSchedule::tick`], which delivers those resources to each [`Facade`]
    /// in parallel batches, if possible.
    pub fn get_facade_schedule(&mut self) -> Result<FacadeSchedule, GraphError> {
        // fetch all the requests for system resources and fold them into a schedule
        let current_iteration = crate::current_iteration();
        let mut i = 0;
        while let Ok(request) = self.facade_requests.try_recv() {
            let node = moongraph::Node::from(request)
                .with_name(format!("request-{}-{}", current_iteration, i));
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
    /// systems, followed by [`World::get_facade_schedule`] and [`RequestSchedule::tick`].
    ///
    /// ```rust, no_run
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
    /// let mut facade_schedule = world.get_facade_schedule().unwrap();
    /// facade_schedule.run().unwrap();
    /// ```
    pub fn run_loop(&mut self) -> Result<&mut Self, GraphError> {
        loop {
            self.tick()?;
            if self.has_facade_requests() {
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
