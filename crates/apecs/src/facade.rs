//! Access the [`World`]s resources from async futures.
use std::{any::Any, sync::Arc};

use moongraph::{Edges, Function, GraphError, Node, Resource, TypeKey, TypeMap};

use crate::{world::LazyOp, World};

pub(crate) struct Request {
    pub(crate) reads: Vec<TypeKey>,
    pub(crate) writes: Vec<TypeKey>,
    pub(crate) moves: Vec<TypeKey>,
    pub(crate) prepare: fn(&mut TypeMap) -> Result<Resource, GraphError>,
    pub(crate) deploy_tx: async_channel::Sender<Resource>,
}

impl From<Request> for Node<Function, TypeKey> {
    fn from(
        Request {
            reads,
            writes,
            moves,
            prepare,
            deploy_tx,
        }: Request,
    ) -> Self {
        Node::new(Function::new(
            prepare,
            move |rez| {
                // Ignore any sending errors as the requesting async could have been
                // dropped while awaiting, which is perfectly legal.
                let _ = deploy_tx.try_send(rez);
                Err(GraphError::TrimNode)
            },
            |_, _| Ok(()),
        ))
        .with_reads(reads)
        .with_writes(writes)
        .with_moves(moves)
    }
}

/// A [`Facade`] visits world resources from async futures.
///
/// A facade is a window into the world, by which an async future can interact
/// with the world's resources through [`Facade::visit`], without causing resource
/// contention with each other or the world's systems.
#[derive(Clone)]
pub struct Facade {
    // Unbounded. Sending a request from the facade will not yield the future.
    // In other words, it will not cause the async to stop at an await point.
    pub(crate) request_tx: async_channel::Sender<Request>,
    pub(crate) lazy_tx: async_channel::Sender<LazyOp>,
}

impl Facade {
    /// Asyncronously visit system resources using a closure.
    ///
    /// The closure may return data to the caller.
    ///
    /// **Note**: Using a closure ensures that no fetched system resources are
    /// held over an await point, which would preclude world systems and other
    /// [`Facade`]s from accessing them and susequently being able to run.
    pub async fn visit<D: Edges + Send + Sync + 'static, T>(
        &mut self,
        f: impl FnOnce(D) -> T,
    ) -> Result<T, GraphError> {
        // request the resources from the world
        let (deploy_tx, deploy_rx) = async_channel::bounded(1);
        let reads = D::reads();
        let writes = D::writes();
        let moves = D::moves();
        // UNWRAP: safe because the request channel is unbounded
        self.request_tx
            .try_send(Request {
                reads,
                writes,
                moves,
                prepare: |resources: &mut TypeMap| {
                    log::debug!(
                        "request got resources - constructing {}",
                        std::any::type_name::<D>()
                    );
                    let my_d = D::construct(resources)?;
                    let my_d_in_a_box: Box<D> = Box::new(my_d);
                    Ok(my_d_in_a_box)
                },
                deploy_tx,
            })
            .unwrap();
        let rez: Resource = deploy_rx.recv().await.map_err(GraphError::other)?;
        // UNWRAP: safe because we know we will only receive the type we expect.
        let box_d: Box<D> = rez.downcast().unwrap();
        let d = *box_d;
        let t = f(d);
        log::debug!("request for {} done", std::any::type_name::<D>());
        Ok(t)
    }

    /// Return the total number of facades.
    pub fn count(&self) -> usize {
        self.request_tx.sender_count()
    }

    pub async fn visit_world_mut<T>(
        &mut self,
        f: impl FnOnce(&mut World) -> T + Send + Sync + 'static,
    ) -> Result<T, GraphError>
    where
        T: Any + Send + Sync,
    {
        let (tx, rx) = async_channel::bounded(1);
        // UNWRAP: safe because this channel is unbounded
        self.lazy_tx
            .try_send(LazyOp {
                op: Box::new(|world| {
                    let t = f(world);
                    let any_t = Arc::new(t);
                    Ok(any_t)
                }),
                tx,
            })
            .unwrap();
        let any_t: Arc<_> = rx.recv().await.map_err(GraphError::other)?;
        // UNWRAP: safe because we know we will only receive the type we expect, as we packed it
        // in the closure above.
        let arc_t: Arc<T> = any_t.downcast().unwrap();
        // UNWRAP: safe because we know nothing has cloned this arc
        Ok(Arc::try_unwrap(arc_t).unwrap_or_else(|_| unreachable!("something cloned the arc")))
    }
}

/// A fulfillment schedule of requests for world resources,
/// coming from all the [`World`](crate::World)'s [`Facade`]s.
pub struct FacadeSchedule<'a> {
    pub(crate) batches: moongraph::Batches<'a>,
}

impl<'a> FacadeSchedule<'a> {
    /// Send out resources for the next batch of the schedule and return `true` when
    /// **either** of these conditions are met:
    /// * request batches are still queued
    /// * resources are still loaned
    pub fn tick(&mut self) -> Result<bool, GraphError> {
        // try to unify
        let resources_unified = self.batches.unify();
        if !resources_unified {
            // we can't run a new batch without unified resources
            log::trace!("cannot run next async request batch - resources still on loan");
            return Ok(true);
        } else {
            log::trace!("ready to run next async request batch");
        }

        if let Some(batch) = self.batches.next_batch() {
            let mut local: Option<fn(Resource) -> Result<Resource, GraphError>> = None;
            let results = batch.run(&mut local)?;
            results.save(true, false)?;
            Ok(true)
        } else {
            log::trace!("async request batches exhausted");
            Ok(false)
        }
    }

    /// Run the schedule by calling [`FacadeSchedule::tick`] in a loop until all
    /// requests are fulfilled and system resources are unified.
    pub fn run(&mut self) -> Result<(), GraphError> {
        while self.tick()? {}
        Ok(())
    }

    /// Return the number of batches left in the schedule.
    pub fn len(&self) -> usize {
        self.batches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Attempt to unify resources, returning `true` if resources are unified, `false` if not.
    pub fn unify(&mut self) -> bool {
        self.batches.unify()
    }
}
