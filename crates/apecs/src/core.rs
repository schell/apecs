//! Core types and processes
use crate::{plugins::Plugin, schedule::Borrow};
use anyhow::Context;
use rayon::iter::IntoParallelIterator;
use rustc_hash::FxHashMap;
use std::{
    any::{Any, TypeId},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};

pub use apecs_derive::{CanFetch, StoredComponent_Range, StoredComponent_Vec};

pub mod oneshot {
    pub use async_oneshot::*;
}

pub mod mpsc {
    pub use async_channel::*;
}

pub mod spsc {
    pub use async_channel::*;
}

pub mod mpmc {
    pub use async_broadcast::*;

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

mod fetch;
pub use fetch::*;

pub trait IsResource: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> IsResource for T {}

pub trait IsComponent: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> IsComponent for T {}

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
    pub fn new<T: IsResource>() -> Self {
        ResourceId {
            type_id: TypeId::of::<T>(),
            name: std::any::type_name::<T>(),
        }
    }

    // pub fn new_storage<T: IsComponent>() -> Self {
    //    ResourceId {
    //        type_id: ResourceTypeId::Storage(TypeId::of::<T>()),
    //        name: std::any::type_name::<T>().to_string(),
    //    }
    //}
}

/// Wrapper for one fetched resource.
///
/// When dropped, the wrapped resource will be sent back to the world.
pub struct Fetched<T: IsResource> {
    // should be unbounded, `send` should never fail
    resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
    inner: Option<Box<T>>,
}

impl<'a, T: IsResource> Deref for Fetched<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource> DerefMut for Fetched<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}

impl<'a, T: IsResource> Drop for Fetched<T> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            self.resource_return_tx
                .try_send((ResourceId::new::<T>(), inner as Resource))
                .unwrap();
        }
    }
}

pub struct Write<T: IsResource + Default>(Fetched<T>);

impl<T: IsResource + Default> Deref for Write<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource + Default> DerefMut for Write<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.inner.as_mut().unwrap()
    }
}

impl<'a, T: Default + Send + Sync + 'static> IntoIterator for &'a Write<T>
where
    &'a T: IntoIterator,
{
    type Item = <<&'a T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().into_iter()
    }
}

impl<'a, T: Default + Send + Sync + 'static> IntoParallelIterator for &'a Write<T>
where
    &'a T: IntoParallelIterator,
{
    type Item = <&'a T as IntoParallelIterator>::Item;

    type Iter = <&'a T as IntoParallelIterator>::Iter;

    fn into_par_iter(self) -> Self::Iter {
        self.deref().into_par_iter()
    }
}

impl<'a, T: Default + Send + Sync + 'static> IntoIterator for &'a mut Write<T>
where
    &'a mut T: IntoIterator,
{
    type Item = <<&'a mut T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref_mut().into_iter()
    }
}

impl<'a, T: Default + Send + Sync + 'static> IntoParallelIterator for &'a mut Write<T>
where
    &'a mut T: IntoParallelIterator,
{
    type Item = <&'a mut T as IntoParallelIterator>::Item;

    type Iter = <&'a mut T as IntoParallelIterator>::Iter;

    fn into_par_iter(self) -> Self::Iter {
        self.deref_mut().into_par_iter()
    }
}

pub struct WriteExpect<T: IsResource>(Fetched<T>);

impl<T: IsResource> Deref for WriteExpect<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource> DerefMut for WriteExpect<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.inner.as_mut().unwrap()
    }
}

impl<'a, S: Send + Sync + 'static> IntoParallelIterator for &'a mut WriteExpect<S>
where
    &'a mut S: IntoParallelIterator,
{
    type Iter = <&'a mut S as IntoParallelIterator>::Iter;

    type Item = <&'a mut S as IntoParallelIterator>::Item;

    fn into_par_iter(self) -> Self::Iter {
        self.0.into_par_iter()
    }
}

impl<'a, S: Send + Sync + 'static> IntoParallelIterator for &'a WriteExpect<S>
where
    &'a S: IntoParallelIterator,
{
    type Iter = <&'a S as IntoParallelIterator>::Iter;
    type Item = <&'a S as IntoParallelIterator>::Item;

    fn into_par_iter(self) -> Self::Iter {
        self.0.into_par_iter()
    }
}

pub struct Read<T: IsResource + Default> {
    inner: Arc<Resource>,
    _phantom: PhantomData<T>,
}

impl<'a, T: IsResource + Default> Deref for Read<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // I don't like this unwrap, but it works
        self.inner.downcast_ref().unwrap()
    }
}

impl<'a, T: Default + Send + Sync + 'static> IntoIterator for &'a Read<T>
where
    &'a T: IntoIterator,
{
    type Item = <<&'a T as IntoIterator>::IntoIter as Iterator>::Item;

    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().into_iter()
    }
}

impl<'a, S: Default + Send + Sync + 'static> IntoParallelIterator for &'a Read<S>
where
    &'a S: IntoParallelIterator,
{
    type Iter = <&'a S as IntoParallelIterator>::Iter;

    type Item = <&'a S as IntoParallelIterator>::Item;

    fn into_par_iter(self) -> Self::Iter {
        self.deref().into_par_iter()
    }
}

pub struct ReadExpect<T: IsResource> {
    inner: Arc<Resource>,
    _phantom: PhantomData<T>,
}

impl<'a, T: IsResource> Deref for ReadExpect<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // I don't like this unwrap, but it works
        self.inner.downcast_ref().unwrap()
    }
}

impl<'a, S: Send + Sync + 'static> IntoParallelIterator for &'a ReadExpect<S>
where
    &'a S: IntoParallelIterator,
{
    type Iter = <&'a S as IntoParallelIterator>::Iter;

    type Item = <&'a S as IntoParallelIterator>::Item;

    fn into_par_iter(self) -> Self::Iter {
        self.deref().into_par_iter()
    }
}

#[derive(Debug, Default)]
pub struct Request {
    pub borrows: Vec<Borrow>,
}

pub struct LazyResource(ResourceId, Box<dyn FnOnce() -> Resource>);

impl LazyResource {
    pub fn new<T: IsResource>(f: impl FnOnce() -> T + 'static) -> LazyResource {
        LazyResource(ResourceId::new::<T>(), Box::new(move || Box::new(f())))
    }

    pub fn id(&self) -> &ResourceId {
        &self.0
    }
}

impl From<LazyResource> for (ResourceId, Resource) {
    fn from(lazy_rez: LazyResource) -> Self {
        (lazy_rez.0, lazy_rez.1())
    }
}

pub enum ResourceRequirement {
    LazyDefault(LazyResource),
    ExpectedExisting(ResourceId),
}

impl ResourceRequirement {
    pub fn id(&self) -> &ResourceId {
        match self {
            ResourceRequirement::LazyDefault(lazy) => &lazy.0,
            ResourceRequirement::ExpectedExisting(id) => &id,
        }
    }

    pub fn is_lazy_default(&self) -> bool {
        matches!(self, ResourceRequirement::LazyDefault(_))
    }
}

/// Types that can be fetched from the [`World`].
pub trait CanFetch: Sized {
    fn borrows() -> Vec<Borrow>;

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self>;

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

impl<'a, T: IsResource + Default> CanFetch for Write<T> {
    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: true,
        }]
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        let WriteExpect(fetched) = WriteExpect::construct(resource_return_tx, fields)?;
        Ok(Write(fetched))
    }

    fn plugin() -> Plugin {
        Plugin::default().with_default_resource::<T>()
    }
}

impl<'a, T: IsResource> CanFetch for WriteExpect<T> {
    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: true,
        }]
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: FetchReadyResource = fields.remove(&id).with_context(|| {
            format!(
                "WriteExpect::construct could not find '{}' in resources",
                std::any::type_name::<T>(),
            )
        })?;
        let t = t.into_owned().context("resource is not owned")?;
        let inner: Option<Box<T>> = Some(t.downcast::<T>().map_err(|_| {
            anyhow::anyhow!(
                "WriteExpect::construct could not cast resource as '{}'",
                std::any::type_name::<T>(),
            )
        })?);
        let fetched = Fetched {
            resource_return_tx,
            inner,
        };
        Ok(WriteExpect(fetched))
    }

    fn plugin() -> Plugin {
        Plugin::default().with_expected_resource::<T>()
    }
}

impl<'a, T: IsResource + Default> CanFetch for Read<T> {
    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: false,
        }]
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        let ReadExpect { inner, .. } = ReadExpect::<T>::construct(resource_return_tx, fields)?;
        Ok(Read {
            inner,
            _phantom: PhantomData,
        })
    }

    fn plugin() -> Plugin {
        Plugin::default().with_default_resource::<T>()
    }
}

impl<'a, T: IsResource> CanFetch for ReadExpect<T> {
    fn borrows() -> Vec<Borrow> {
        vec![Borrow {
            id: ResourceId::new::<T>(),
            is_exclusive: false,
        }]
    }

    fn construct(
        _: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: FetchReadyResource = fields.remove(&id).with_context(|| {
            format!(
                "ReadExpect::construct could not find '{}' in resources",
                std::any::type_name::<T>(),
            )
        })?;
        let inner = t.into_ref().context("resource is not borrowed")?;

        Ok(ReadExpect {
            inner,
            _phantom: PhantomData,
        })
    }

    fn plugin() -> Plugin {
        Plugin::default().with_expected_resource::<T>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
