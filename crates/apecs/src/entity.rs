use std::{
    any::{Any, TypeId},
    collections::VecDeque,
    ops::Deref,
    sync::Arc,
};

use moongraph::GraphError;

use crate::{world::LazyOp, Entry, IsBundle, IsQuery, World};

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
    pub(crate) fn new(lazy_op_sender: async_channel::Sender<LazyOp>) -> Self {
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
