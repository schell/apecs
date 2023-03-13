use std::{future::Future, pin::Pin, sync::atomic::AtomicU64};

use anyhow::Context;
use dagga::Node;
use itertools::Itertools;

use crate::{
    internal::{TypeKey, TypeValue},
    resource_manager::LoanManager,
    CanFetch,
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
pub(crate) fn increment_current_iteration() -> u64 {
    SYSTEM_ITERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A future representing an async system.
pub type AsyncSystemFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + Sync + 'static>>;

/// Whether or not a system should continue execution.
pub enum ShouldContinue {
    Yes,
    No,
}

/// Returns a syncronous system result meaning everything is ok and the system
/// should run again next frame.
pub fn ok() -> anyhow::Result<ShouldContinue> {
    Ok(ShouldContinue::Yes)
}

/// Returns a syncronous system result meaning everything is ok, but the system
/// should not be run again.
pub fn end() -> anyhow::Result<ShouldContinue> {
    Ok(ShouldContinue::No)
}

/// Returns a syncronous system result meaning an error occured and the system
/// should not run again.
pub fn err(err: anyhow::Error) -> anyhow::Result<ShouldContinue> {
    Err(err)
}

pub type SystemFunction =
    Box<dyn FnMut(TypeValue) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static>;

pub struct System {
    pub prepare: fn(&mut LoanManager<'_>) -> anyhow::Result<TypeValue>,
    pub function: SystemFunction,
}

impl System {
    pub fn run(&mut self, data: TypeValue) -> anyhow::Result<ShouldContinue> {
        (self.function)(data)
    }

    pub fn node<T, F>(
        name: impl AsRef<str>,
        mut sys_fn: F,
        runs_before: &[&str],
        runs_after: &[&str],
    ) -> Node<Self, TypeKey>
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch + Send + Sync + 'static,
    {
        let (writes, reads): (Vec<_>, Vec<_>) = T::borrows().into_iter().partition_map(|borrow| {
            if borrow.is_exclusive() {
                itertools::Either::Left(borrow.rez_id())
            } else {
                itertools::Either::Right(borrow.rez_id())
            }
        });

        let system = System {
            prepare: |loan_mngr: &mut LoanManager| {
                let rez: TypeValue = TypeValue::from(Box::new(T::construct(loan_mngr)?));
                Ok(rez)
            },
            function: Box::new(move |b: TypeValue| {
                let box_t: Box<T> = b.downcast().ok().context("cannot downcast")?;
                let t: T = *box_t;
                sys_fn(t)
            }),
        };

        let mut node = dagga::Node::new(system)
            .with_name(name.as_ref())
            .with_reads(reads)
            .with_writes(writes);
        node = runs_before
            .iter()
            .fold(node, |node, sys| node.run_before(sys.to_string()));
        node = runs_after
            .iter()
            .fold(node, |node, sys| node.run_after(sys.to_string()));
        node
    }
}
