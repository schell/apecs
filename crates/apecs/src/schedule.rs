//! System scheduling for outer parallelism.
//!
//! This module contains trait definitions. Implementations can be found in
//! other modeluse.
use anyhow::Context;
use dagga::{Dag, Node, Schedule};
use itertools::Itertools;
use rayon::prelude::*;
use std::any::TypeId;

use crate::{
    internal::Resource,
    system::{self, System},
    Request,
};

use super::{
    resource_manager::{LoanManager, ResourceManager},
    system::ShouldContinue,
};

#[derive(Default)]
pub struct SystemSchedule {
    num_threads: u32,
    current_barrier: usize,
    pub(crate) unscheduled_systems: Vec<Node<System, TypeId>>,
    pub(crate) scheduled_systems: Vec<Vec<Node<System, TypeId>>>,
}

impl SystemSchedule {
    pub fn set_parallelism(&mut self, threads: u32) {
        self.num_threads = threads;
    }

    pub fn add_barrier(&mut self) {
        self.current_barrier += 1;
    }

    pub fn add_system(&mut self, mut node: Node<System, TypeId>) {
        node.set_barrier(self.current_barrier);
        self.unscheduled_systems.push(node);
    }

    pub fn contains_system(&self, name: impl AsRef<str>) -> bool {
        if self
            .scheduled_systems
            .iter()
            .flat_map(|batch| batch)
            .any(|node| node.name() == name.as_ref())
        {
            return true;
        } else {
            self.unscheduled_systems
                .iter()
                .any(|node| node.name() == name.as_ref())
        }
    }

    pub fn is_empty(&self) -> bool {
        self.unscheduled_systems.is_empty() && self.scheduled_systems.is_empty()
    }

    pub fn reschedule(&mut self) -> anyhow::Result<()> {
        let previously_scheduled_systems = std::mem::take(&mut self.scheduled_systems)
            .into_iter()
            .flat_map(|batch| batch);
        let unscheduled_systems = std::mem::take(&mut self.unscheduled_systems);
        let dag = previously_scheduled_systems
            .chain(unscheduled_systems)
            .fold(Dag::<System, TypeId>::default(), Dag::with_node);
        self.scheduled_systems = dag
            .build_schedule()
            .context("can't build schedule")?
            .batches;
        Ok(())
    }

    pub fn run(&mut self, resource_manager: &mut ResourceManager) -> anyhow::Result<()> {
        if !self.unscheduled_systems.is_empty() {
            // we have unscheduled systems so we must first (re)generate the schedule with
            // the new systems
            self.reschedule()?;
        }

        for (i, batch) in std::mem::take(&mut self.scheduled_systems)
            .into_iter()
            .enumerate()
        {
            // prepare loans for the reads and writes needed by each system in the batch
            let mut loan_mngr = LoanManager(resource_manager);
            let mut data = vec![];
            for node in batch.iter() {
                data.push((node.inner().prepare)(&mut loan_mngr)?);
            }

            let (remaining_systems, errs): (Vec<_>, Vec<_>) = if self.num_threads > 1 {
                let available_threads = rayon::current_num_threads();
                if self.num_threads > available_threads as u32 {
                    log::warn!(
                        "the rayon threadpool does not contain enough threads! requested {} for \
                         batch {i}, have {available_threads}",
                        self.num_threads,
                    );
                }
                (batch, data)
                    .into_par_iter()
                    .filter_map(|(mut system_node, data)| {
                        log::trace!("running par system '{}'", system_node.name());
                        let _ = system::increment_current_iteration();
                        match system_node.inner_mut().run(data) {
                            Ok(ShouldContinue::Yes) => Some(rayon::iter::Either::Left(system_node)),
                            Ok(ShouldContinue::No) => None,
                            Err(err) => Some(rayon::iter::Either::Right(err)),
                        }
                    })
                    .partition_map(|e| e)
            } else {
                let mut remaining_systems = vec![];
                let mut errs = vec![];
                batch
                    .into_iter()
                    .zip(data.into_iter())
                    .for_each(|(mut system_node, data)| {
                        log::trace!("running system '{}'", system_node.name());
                        let _ = system::increment_current_iteration();
                        match system_node.inner_mut().run(data) {
                            Ok(ShouldContinue::Yes) => {
                                remaining_systems.push(system_node);
                            }
                            Ok(ShouldContinue::No) => {}
                            Err(err) => {
                                errs.push(err);
                            }
                        }
                    });
                (remaining_systems, errs)
            };

            if !remaining_systems.is_empty() {
                self.scheduled_systems.push(remaining_systems);
            }

            drop(loan_mngr);
            resource_manager.unify_resources("system schedule batch run")?;

            errs.into_iter()
                .fold(Ok(()), |may_err, err| match may_err {
                    Ok(()) => Err(err),
                    Err(prev) => Err(prev.context(format!("and {}", err))),
                })?;
        }

        Ok(())
    }
}

/// A fulfillment schedule of requests of world resources, coming from the [`World`]'s
/// [`Facade`]s.
pub struct FacadeSchedule<'a> {
    requests: Vec<Request>,
    schedule: Schedule<Request>,
    resource_manager: &'a mut ResourceManager,
}

impl<'a> FacadeSchedule<'a> {
    pub fn new(resource_manager: &'a mut ResourceManager) -> Self {
        Self {
            requests: vec![],
            schedule: Schedule { batches: vec![] },
            resource_manager,
        }
    }

    pub fn add_request(&mut self, req: Request) {
        self.requests.push(req);
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    /// Collects all incoming requests into system batches and schedules a DAG for their
    /// execution.
    pub fn reschedule(&mut self) -> anyhow::Result<()> {
        let mut requests = self.schedule.batches.drain(0..).concat();
        requests.extend(self.requests.drain(0..));
        let mut dag: Dag<Request, TypeId> = Dag::default();

        for (i, req) in requests.into_iter().enumerate() {
            let (writes, reads): (Vec<_>, Vec<_>) = req.borrows.iter().partition_map(|borrow| {
                if borrow.is_exclusive() {
                    itertools::Either::Left(borrow.rez_id().type_id)
                } else {
                    itertools::Either::Right(borrow.rez_id().type_id)
                }
            });
            dag.add_node(
                Node::new(req)
                    .with_name(format!("request-{}", i))
                    .with_reads(reads)
                    .with_writes(writes),
            );
        }

        let schedule: Schedule<Node<Request, TypeId>> = dag.build_schedule().unwrap();
        self.schedule = schedule.map(|node| node.into_inner());
        Ok(())
    }

    /// Pop off the next batch in the schedule.
    pub fn next_batch(&mut self) -> Option<Vec<Request>> {
        if self.schedule.batches.is_empty() {
            None
        } else {
            self.schedule.batches.drain(0..1).next()
        }
    }

    /// Sends out resources to fulfill the given batch of `Request`s, if possible.  
    pub fn run_batch(&mut self, batch: Vec<Request>) -> anyhow::Result<()> {
        let resources_still_loaned = self
            .resource_manager
            .try_unify_resources("run async batch")?;
        if resources_still_loaned {
            self.schedule.batches.insert(0, batch);
            anyhow::bail!("an async system is holding onto resources over an await point!");
        }

        let batch_len = batch.len();
        let mut loan_mngr = LoanManager(self.resource_manager);
        for (j, req) in batch.into_iter().enumerate() {
            // construct the response to each request
            let data: Resource = (req.construct)(&mut loan_mngr)?;
            // send the resources off, if need be
            if !req.deploy_tx.is_closed() {
                log::trace!(
                    "sending resource '{}' to a batch async request {j}/{batch_len}",
                    data.type_name().unwrap_or("unknown"),
                );
                // UNWRAP: safe because we checked above that the channel is still open
                req.deploy_tx.try_send(data).unwrap();
            } else {
                log::trace!(
                    "cancelling send of resource '{}' to batch, async request {j}",
                    data.type_name().unwrap_or("unknown"),
                );
            }
        }

        Ok(())
    }

    /// Send out resources for the next batch of the schedule and return whether there are
    /// more batches to run.
    pub fn tick(&mut self) -> anyhow::Result<bool> {
        if let Some(batch) = self.next_batch() {
            self.run_batch(batch)?;
        }
        Ok(!self.schedule.batches.is_empty())
    }
}
