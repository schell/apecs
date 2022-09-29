//! *A*syncronous and *P*leasant *E*ntity *C*omponent *S*ystem
#![allow(clippy::type_complexity)]

use ::anyhow::Context;
use internal::{FetchReadyResource, LoanManager, Resource, ResourceId};
use rayon::iter::IntoParallelIterator;
use std::{
    any::Any,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};

pub mod anyhow {
    //! Re-export of the anyhow error handling library.
    pub use anyhow::*;
}

mod fetch;
mod plugin;
mod resource_manager;
mod schedule;
mod storage;
mod system;
mod world;

#[cfg(feature = "derive")]
pub use apecs_derive::CanFetch;
#[cfg(feature = "derive")]
pub use apecs_derive::TryDefault;
pub use fetch::*;
pub use plugin::Plugin;
pub use rustc_hash::FxHashMap;
pub use storage::{
    Components, Entry, IsBundle, IsQuery, Maybe, MaybeMut, MaybeRef, Mut, Query, QueryGuard,
    QueryIter, Ref, Without,
};
pub use system::{current_iteration, end, err, ok, ShouldContinue};
pub use world::{Entities, Entity, Facade, Parallelism, World};

#[cfg(doctest)]
pub mod doctest {
    #[doc = include_str!("../../../README.md")]
    pub struct ReadmeDoctests;
}

pub mod chan {
    //! A few flavors of channels to help writing async code
    pub mod oneshot {
        //! Oneshot channel
        pub use async_oneshot::*;
    }

    pub mod mpsc {
        //! Multiple producer, single consumer channel
        pub use async_channel::*;
    }

    pub mod spsc {
        //! Single producer, single consumer channel
        pub use async_channel::*;
    }

    pub mod mpmc {
        //! Multiple producer, multiple consumer "broadcast" channel
        pub use async_broadcast::*;

        /// A broadcast channel with an inactive receiver
        #[derive(Clone)]
        pub struct Channel<T> {
            pub tx: Sender<T>,
            pub rx: InactiveReceiver<T>,
        }

        impl<T: Clone> Channel<T> {
            pub fn new_with_capacity(cap: usize) -> Self {
                let (mut tx, rx) = broadcast(cap);
                tx.set_overflow(true);
                let rx = rx.deactivate();
                Channel { tx, rx }
            }

            pub fn new_receiver(&self) -> Receiver<T> {
                self.rx.activate_cloned()
            }

            pub fn try_send(&mut self, msg: T) -> anyhow::Result<()> {
                match self.tx.try_broadcast(msg) {
                    Ok(me) => match me {
                        Some(e) => {
                            self.tx.set_capacity(self.tx.capacity() + 1);
                            self.try_send(e)
                        }
                        None => Ok(()),
                    },
                    Err(e) => match e {
                        // nobody is listening so it doesn't matter
                        TrySendError::Inactive(_) => Ok(()),
                        _ => Err(anyhow::anyhow!("{}", e)),
                    },
                }
            }
        }
    }
}

pub mod internal {
    //! Types used internally for deriving [`CanFetch`](crate::CanFetch) and
    //! other macros.
    use std::any::TypeId;
    use std::{any::Any, sync::Arc};

    pub use super::resource_manager::LoanManager;
    pub use super::schedule::Borrow;

    /// A type-erased resource.
    pub type Resource = Box<dyn Any + Send + Sync + 'static>;

    /// A resource that is ready for fetching, which means it is
    /// either owned (and therefore mutable by the owner) or sitting in an Arc.
    pub enum FetchReadyResource {
        Owned(Resource),
        Ref(Arc<Resource>),
    }

    impl std::fmt::Debug for FetchReadyResource {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Owned(_) => f.debug_tuple("Owned").field(&"_").finish(),
                Self::Ref(_) => f.debug_tuple("Ref").field(&"_").finish(),
            }
        }
    }

    impl FetchReadyResource {
        pub fn into_owned(self) -> Option<Resource> {
            match self {
                FetchReadyResource::Owned(r) => Some(r),
                FetchReadyResource::Ref(_) => None,
            }
        }

        pub fn into_ref(self) -> Option<Arc<Resource>> {
            match self {
                FetchReadyResource::Owned(_) => None,
                FetchReadyResource::Ref(r) => Some(r),
            }
        }

        pub fn is_owned(&self) -> bool {
            matches!(self, FetchReadyResource::Owned(_))
        }

        pub fn is_ref(&self) -> bool {
            !self.is_owned()
        }
    }

    #[derive(Clone, Debug, Eq)]
    pub struct ResourceId {
        pub(crate) type_id: TypeId,
        // TODO: Hide this unless debug-assertions
        pub(crate) name: &'static str,
    }

    impl std::fmt::Display for ResourceId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.name)
        }
    }

    impl std::hash::Hash for ResourceId {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.type_id.hash(state);
        }
    }

    impl PartialEq for ResourceId {
        fn eq(&self, other: &Self) -> bool {
            self.type_id == other.type_id
        }
    }

    impl ResourceId {
        pub fn new<T: Any + Send + Sync + 'static>() -> Self {
            ResourceId {
                type_id: TypeId::of::<T>(),
                name: std::any::type_name::<T>(),
            }
        }
    }
}

/// Marker trait that denotes a static, nameable type that can be sent between
/// threads.
pub trait IsResource: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> IsResource for T {}

/// Optional default creation.
///
/// This is used to attempt to create resources when they don't already exist
/// in the world.
pub trait TryDefault: Sized {
    fn try_default() -> Option<Self> {
        None
    }
}

/// Wrapper for one fetched resource.
///
/// When dropped, the wrapped resource will be sent back to the world.
pub(crate) struct Fetched<T: IsResource + TryDefault> {
    // should be unbounded, `send` should never fail
    resource_return_tx: chan::mpsc::Sender<(ResourceId, Resource)>,
    inner: Option<Box<T>>,
}

impl<'a, T: IsResource + TryDefault> Deref for Fetched<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource + TryDefault> DerefMut for Fetched<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl<'a, T: IsResource + TryDefault> Drop for Fetched<T> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            self.resource_return_tx
                .try_send((ResourceId::new::<T>(), inner as Resource))
                .unwrap();
        }
    }
}

/// A mutably borrowed resource that can be created by default.
///
/// The resource is automatically sent back to the world on drop.
pub struct Write<T: IsResource + TryDefault>(Fetched<T>);

impl<T: IsResource + TryDefault> Deref for Write<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource + TryDefault> DerefMut for Write<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.inner.as_mut().unwrap()
    }
}

impl<'a, T: TryDefault + Send + Sync + 'static> IntoIterator for &'a Write<T>
where
    &'a T: IntoIterator,
{
    type Item = <<&'a T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().into_iter()
    }
}

impl<'a, T: TryDefault + Send + Sync + 'static> IntoParallelIterator for &'a Write<T>
where
    &'a T: IntoParallelIterator,
{
    type Item = <&'a T as IntoParallelIterator>::Item;

    type Iter = <&'a T as IntoParallelIterator>::Iter;

    fn into_par_iter(self) -> Self::Iter {
        self.deref().into_par_iter()
    }
}

impl<'a, T: TryDefault + Send + Sync + 'static> IntoIterator for &'a mut Write<T>
where
    &'a mut T: IntoIterator,
{
    type Item = <<&'a mut T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref_mut().into_iter()
    }
}

impl<'a, T: TryDefault + Send + Sync + 'static> IntoParallelIterator for &'a mut Write<T>
where
    &'a mut T: IntoParallelIterator,
{
    type Item = <&'a mut T as IntoParallelIterator>::Item;

    type Iter = <&'a mut T as IntoParallelIterator>::Iter;

    fn into_par_iter(self) -> Self::Iter {
        self.deref_mut().into_par_iter()
    }
}

impl<T: IsResource + TryDefault> Write<T> {
    fn inner(&self) -> &T {
        self.deref()
    }

    fn inner_mut(&mut self) -> &mut T {
        self.deref_mut()
    }
}

/// Immutably borrowed resource that can be created by default.
///
/// The resource is automatically sent back to the world on drop.
pub struct Read<T: IsResource + TryDefault> {
    inner: Arc<Resource>,
    _phantom: PhantomData<T>,
}

impl<'a, T: IsResource + TryDefault> Deref for Read<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.downcast_ref().unwrap()
    }
}

impl<'a, T: TryDefault + Send + Sync + 'static> IntoIterator for &'a Read<T>
where
    &'a T: IntoIterator,
{
    type Item = <<&'a T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().into_iter()
    }
}

impl<'a, S: TryDefault + Send + Sync + 'static> IntoParallelIterator for &'a Read<S>
where
    &'a S: IntoParallelIterator,
{
    type Iter = <&'a S as IntoParallelIterator>::Iter;

    type Item = <&'a S as IntoParallelIterator>::Item;

    fn into_par_iter(self) -> Self::Iter {
        self.deref().into_par_iter()
    }
}

impl<T: IsResource + TryDefault> Read<T> {
    fn inner(&self) -> &T {
        self.deref()
    }
}

pub(crate) struct Request {
    pub borrows: Vec<internal::Borrow>,
    pub construct: fn(&mut LoanManager<'_>) -> anyhow::Result<Resource>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("borrows", &self.borrows)
            .field(
                "construct",
                &"fn(&mut internal::LoanManager<'_> -> anyhow::Result<Resource>)",
            )
            .finish()
    }
}

impl Default for Request {
    fn default() -> Self {
        Self {
            borrows: Default::default(),
            construct: |_| anyhow::bail!("default construct"),
        }
    }
}

pub(crate) struct LazyResource {
    id: ResourceId,
    create: Box<dyn FnOnce(&mut LoanManager) -> anyhow::Result<Resource>>,
}

impl LazyResource {
    pub fn new<T: IsResource>(
        f: impl FnOnce(&mut LoanManager) -> anyhow::Result<T> + 'static,
    ) -> LazyResource {
        LazyResource{
            id: ResourceId::new::<T>(),
            create: Box::new(move |loans| Ok(Box::new(f(loans)?))),
        }
    }
}

/// Types that can be fetched from the [`World`].
pub trait CanFetch: Send + Sync + Sized {
    fn borrows() -> Vec<internal::Borrow>;

    /// Attempt to construct `Self` with the given `LoanManager`.
    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self>;

    /// Return a plugin containing the systems and sub-resources required to
    /// create and use the type.
    ///
    /// This will be used by functions like [`World::with_plugin`] to ensure
    /// that a type's resources have been created, and that the systems
    /// required for upkeep are included.
    fn plugin() -> Plugin {
        Plugin::default()
    }
}

impl<'a, T: IsResource + TryDefault> CanFetch for Write<T> {
    fn borrows() -> Vec<internal::Borrow> {
        vec![internal::Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: true,
        }]
    }

    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self> {
        let borrow = internal::Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: true,
        };
        let t: FetchReadyResource =
            loan_mngr.get_loaned_or_try_default::<T>("Write::construct", &borrow)?;
        let t = t.into_owned().context("resource is not owned")?;
        let inner: Option<Box<T>> = Some(t.downcast::<T>().map_err(|_| {
            anyhow::anyhow!(
                "Write::construct could not cast resource as '{}'",
                std::any::type_name::<T>(),
            )
        })?);
        let fetched = Fetched {
            resource_return_tx: loan_mngr.resource_return_tx(),
            inner,
        };
        Ok(Write(fetched))
    }

    fn plugin() -> Plugin {
        Plugin::default().with_resource::<T>()
    }
}

impl<'a, T: IsResource + TryDefault> CanFetch for Read<T> {
    fn borrows() -> Vec<internal::Borrow> {
        vec![internal::Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: false,
        }]
    }

    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self> {
        let borrow = internal::Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: false,
        };
        let t: FetchReadyResource =
            loan_mngr.get_loaned_or_try_default::<T>("Read::construct", &borrow)?;
        let inner = t.into_ref().context("resource is not borrowed")?;
        Ok(Read {
            inner,
            _phantom: PhantomData,
        })
    }

    fn plugin() -> Plugin {
        Plugin::default().with_resource::<T>()
    }
}

#[cfg(test)]
mod tests {
    use super::chan::mpsc;

    #[test]
    fn unbounded_channel_doesnt_yield_on_send_and_await() {
        let (tx, rx) = mpsc::unbounded::<u32>();

        let executor = async_executor::Executor::new();
        let _t = executor.spawn(async move {
            for i in 0..5u32 {
                tx.send(i).await.unwrap();
            }
        });

        assert!(executor.try_tick());

        let mut msgs = vec![];
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }

        assert_eq!(msgs, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn executor_sanity() {
        let (tx, rx) = mpsc::bounded::<String>(1);

        let executor = async_executor::Executor::new();
        let tx_t = tx.clone();
        let _t = executor.spawn(async move {
            let mut n = 0;
            loop {
                tx_t.send(format!("A {}", n)).await.unwrap();
                n += 1;
            }
        });

        let tx_s = tx;
        let _s = executor.spawn(async move {
            let mut n = 0;
            loop {
                tx_s.send(format!("B {}", n)).await.unwrap();
                n += 1;
            }
        });

        let mut msgs = vec![];
        for _ in 0..10 {
            msgs.push("tick".to_string());
            let _ = executor.try_tick();
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
        }

        let (a, _b) = msgs.split_at(4);
        assert_eq!(
            a,
            &[
                "tick".to_string(),
                "A 0".to_string(),
                "tick".to_string(),
                "B 0".to_string()
            ]
        );
    }
}
