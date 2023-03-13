//! Welcome to the docs of the *A*syncronous and *P*leasant *E*ntity *C*omponent
//! *S*ystem 😊
//!
//! `apecs` is a flexible and well-performing entity-component-system libary
//! that includes support for asynchronous systems.
//!
//! It's best to start learning about `apecs` from the [`World`], but if you
//! just want some examples please check out the [readme](https://github.com/schell/apecs#readme)
#![allow(clippy::type_complexity)]

use ::anyhow::Context;
use broomdog::Loan;
use chan::spsc;
use internal::{FetchReadyResource, LoanManager, TypeKey, TypeValue};
use rayon::iter::IntoParallelIterator;
use std::{
    any::Any,
    marker::PhantomData,
    ops::{Deref, DerefMut},
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
pub use async_executor::Task;
#[cfg(feature = "derive")]
pub use fetch::*;
pub use plugin::Plugin;
pub use rustc_hash::FxHashMap;
pub use storage::{
    Components, Entry, IsBundle, IsQuery, LazyComponents, Maybe, MaybeMut, MaybeRef, Mut, Query,
    QueryGuard, QueryIter, Ref, Without,
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

        /// By default a channel is created with a capacity of `1`.
        impl<T: Clone> Default for Channel<T> {
            fn default() -> Self {
                Self::new_with_capacity(1)
            }
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
    use broomdog::{Loan, LoanMut};
    pub use broomdog::{TypeKey, TypeValue};

    pub use super::resource_manager::LoanManager;

    /// Describes borrowing of system resources at runtime. For internal use,
    /// mostly.
    #[derive(Clone, Debug)]
    pub struct Borrow {
        pub id: TypeKey,
        pub is_exclusive: bool,
    }

    impl Borrow {
        /// The resource id
        pub fn rez_id(&self) -> TypeKey {
            self.id.clone()
        }

        /// The type name of the resource
        pub fn name(&self) -> &str {
            self.id.name()
        }

        /// Whether this borrow is mutable (`true`) or immutable (`false`).
        pub fn is_exclusive(&self) -> bool {
            self.is_exclusive
        }
    }

    /// A resource that is ready for fetching, which means it is
    /// either owned (and therefore mutable by the owner) or sitting in an Arc.
    pub enum FetchReadyResource {
        Owned(LoanMut),
        Ref(Loan),
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
        pub fn into_owned(self) -> Option<LoanMut> {
            match self {
                FetchReadyResource::Owned(r) => Some(r),
                FetchReadyResource::Ref(_) => None,
            }
        }

        pub fn into_ref(self) -> Option<Loan> {
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
}

/// Marker trait that denotes a static, nameable type that can be sent between
/// threads.
pub trait IsResource: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> IsResource for T {}

/// Used to generate a default value of a resource, if possible.
pub trait Gen<T> {
    fn generate() -> Option<T>;
}

/// Valueless type that represents the ability to generate a resource by
/// default.
pub struct SomeDefault;

impl<T: Default> Gen<T> for SomeDefault {
    fn generate() -> Option<T> {
        Some(T::default())
    }
}

/// Valueless type that represents the **inability** to generate a resource by
/// default.
pub struct NoDefault;

impl<T> Gen<T> for NoDefault {
    fn generate() -> Option<T> {
        None
    }
}

/// A mutably borrowed resource that may be created by default.
///
/// [`Read`] and [`Write`] are the main way systems interact with resources.
/// When [`fetch`](crate::World::fetch)ed the wrapped type `T` will
/// automatically be created by default. If fetched as `Write<T, NoDefault>` the
/// fetch will err if the resource doesn't already exist.
///
/// After a successful fetch, the resource will be automatically sent back to
/// the world on drop. To make sure that your async functions don't hold fetched
/// resources over await points, [`Facade`] uses [`visit`](Facade::visit) which
/// fetches inside a syncronous closure.
///
/// `Write` has two type parameters:
/// * `T` - The type of the resource.
/// * `G` - The method by which the resource can be generated if it doesn't
///   already exist. By default this is [`SomeDefault`], which denotes creating
///   the resource using its default implementation. Another option is
///   [`NoDefault`] which fails to generate the resource.
///
/// ```rust
/// use apecs::*;
///
/// let mut world = World::default();
/// {
///     let default_number = world.fetch::<Read<u32>>();
///     assert_eq!(Some(0), default_number.map(|n| *n).ok());
/// }
/// {
///     let no_number = world.fetch::<Read<f32, NoDefault>>();
///     assert!(no_number.is_err());
/// }
/// ```
pub struct Write<T: IsResource, G: Gen<T> = SomeDefault> {
    inner: broomdog::LoanMut,
    _t: PhantomData<T>,
    _g: PhantomData<G>,
}

impl<T: IsResource, G: Gen<T>> Deref for Write<T, G> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.downcast_ref().unwrap()
    }
}

impl<T: IsResource, G: Gen<T>> DerefMut for Write<T, G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.downcast_mut().unwrap()
    }
}

impl<'a, T: Send + Sync + 'static, G: Gen<T>> IntoIterator for &'a Write<T, G>
where
    &'a T: IntoIterator,
{
    type Item = <<&'a T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().into_iter()
    }
}

impl<'a, T: Send + Sync + 'static, G: Gen<T>> IntoParallelIterator for &'a Write<T, G>
where
    &'a T: IntoParallelIterator,
{
    type Item = <&'a T as IntoParallelIterator>::Item;

    type Iter = <&'a T as IntoParallelIterator>::Iter;

    fn into_par_iter(self) -> Self::Iter {
        self.deref().into_par_iter()
    }
}

impl<'a, T: Send + Sync + 'static, G: Gen<T>> IntoIterator for &'a mut Write<T, G>
where
    &'a mut T: IntoIterator,
{
    type Item = <<&'a mut T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref_mut().into_iter()
    }
}

impl<'a, T: Send + Sync + 'static, G: Gen<T>> IntoParallelIterator for &'a mut Write<T, G>
where
    &'a mut T: IntoParallelIterator,
{
    type Item = <&'a mut T as IntoParallelIterator>::Item;

    type Iter = <&'a mut T as IntoParallelIterator>::Iter;

    fn into_par_iter(self) -> Self::Iter {
        self.deref_mut().into_par_iter()
    }
}

impl<T: IsResource, G: Gen<T>> Write<T, G> {
    /// An explicit method of getting a reference to the inner type without
    /// `Deref`.
    pub fn inner(&self) -> &T {
        self.deref()
    }

    /// An explicit method of getting a mutable reference to the inner type
    /// without `DerefMut`.
    pub fn inner_mut(&mut self) -> &mut T {
        self.deref_mut()
    }
}

/// Immutably borrowed resource that may be created by default.
///
/// [`Read`] and [`Write`] are the main way systems interact with resources.
/// When [`fetch`](crate::World::fetch)ed The wrapped type `T` will
/// automatically be created b default. If fetched as `Write<T, NoDefault>` the
/// fetch will err if the resource doesn't already exist.
///
/// After a successful fetch, the resource will be automatically sent back to
/// the world on drop.
///
/// `Read` has two type parameters:
/// * `T` - The type of the resource.
/// * `G` - The method by which the resource can be generated if it doesn't
///   exist. By default this is [`SomeDefault`], which denotes creating the
///   resource using its default instance. Another option is [`NoDefault`] which
///   fails to generate the resource.
///
/// ```rust
/// use apecs::*;
///
/// let mut world = World::default();
/// {
///     let default_number = world.fetch::<Read<u32>>();
///     assert_eq!(Some(0), default_number.map(|n| *n).ok());
/// }
/// {
///     let no_number = world.fetch::<Read<f32, NoDefault>>();
///     assert!(no_number.is_err());
/// }
/// ```
pub struct Read<T: IsResource, G: Gen<T> = SomeDefault> {
    inner: Loan,
    _phantom_t: PhantomData<T>,
    _phantom_g: PhantomData<G>,
}

impl<'a, T: IsResource, G: Gen<T>> Deref for Read<T, G> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.downcast_ref().unwrap()
    }
}

impl<'a, T: Send + Sync + 'static, G: Gen<T>> IntoIterator for &'a Read<T, G>
where
    &'a T: IntoIterator,
{
    type Item = <<&'a T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().into_iter()
    }
}

impl<'a, S: Send + Sync + 'static, G: Gen<S>> IntoParallelIterator for &'a Read<S, G>
where
    &'a S: IntoParallelIterator,
{
    type Iter = <&'a S as IntoParallelIterator>::Iter;

    type Item = <&'a S as IntoParallelIterator>::Item;

    fn into_par_iter(self) -> Self::Iter {
        self.deref().into_par_iter()
    }
}

impl<T: IsResource, G: Gen<T>> Read<T, G> {
    /// An explicit method of getting a reference to the inner type without
    /// `Deref`.
    pub fn inner(&self) -> &T {
        self.deref()
    }
}

pub struct Request {
    pub borrows: Vec<internal::Borrow>,
    pub construct: fn(&mut LoanManager<'_>) -> anyhow::Result<TypeValue>,
    pub deploy_tx: spsc::Sender<TypeValue>,
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

pub(crate) struct LazyResource {
    id: TypeKey,
    create: Box<dyn FnOnce(&mut LoanManager) -> anyhow::Result<TypeValue>>,
}

impl LazyResource {
    pub fn new<T: IsResource>(
        f: impl FnOnce(&mut LoanManager) -> anyhow::Result<T> + 'static,
    ) -> LazyResource {
        LazyResource {
            id: TypeKey::new::<T>(),
            create: Box::new(move |loans| Ok(TypeValue::from(Box::new(f(loans)?)))),
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

impl<'a, T: IsResource, G: Gen<T> + IsResource> CanFetch for Write<T, G> {
    fn borrows() -> Vec<internal::Borrow> {
        vec![internal::Borrow {
            id: TypeKey::new::<T>(),
            is_exclusive: true,
        }]
    }

    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self> {
        let borrow = internal::Borrow {
            id: TypeKey::new::<T>(),
            is_exclusive: true,
        };
        let t: FetchReadyResource =
            loan_mngr.get_loaned_or_gen::<T, G>("Write::construct", &borrow)?;
        let inner = t.into_owned().context("resource is not owned")?;
        Ok(Write {
            inner,
            _t: PhantomData,
            _g: PhantomData,
        })
    }

    fn plugin() -> Plugin {
        Plugin::default().with_resource(|_: ()| {
            G::generate().with_context(|| {
                format!(
                    "Write could not generate {} with '{}'",
                    std::any::type_name::<T>(),
                    std::any::type_name::<G>()
                )
            })
        })
    }
}

impl<'a, T: IsResource, G: Gen<T> + IsResource> CanFetch for Read<T, G> {
    fn borrows() -> Vec<internal::Borrow> {
        vec![internal::Borrow {
            id: TypeKey::new::<T>(),
            is_exclusive: false,
        }]
    }

    fn construct(loan_mngr: &mut LoanManager) -> anyhow::Result<Self> {
        let borrow = internal::Borrow {
            id: TypeKey::new::<T>(),
            is_exclusive: false,
        };
        let t: FetchReadyResource =
            loan_mngr.get_loaned_or_gen::<T, G>("Read::construct", &borrow)?;
        let inner = t.into_ref().context("resource is not borrowed")?;
        Ok(Read {
            inner,
            _phantom_t: PhantomData,
            _phantom_g: PhantomData,
        })
    }

    fn plugin() -> Plugin {
        Plugin::default().with_resource(|_: ()| {
            G::generate().with_context(|| {
                format!(
                    "Read could not generate '{}' with '{}'",
                    std::any::type_name::<T>(),
                    std::any::type_name::<G>()
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::chan::mpsc;
    use crate::{World, Write};

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

    #[test]
    fn can_compile_write_without_trydefault() {
        let mut world = World::default();
        world.with_resource(0.0f32).unwrap();
        {
            let _f32_num = world.resource_mut::<f32>().unwrap();
        }
        {
            let _f32_num = world.fetch::<Write<f32>>().unwrap();
        }
    }
}
