//! Core types and processes
use std::{
    any::{Any, TypeId},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use anyhow::Context;
use rustc_hash::FxHashMap;

pub use apecs_derive::CanFetch;

pub mod mpsc {
    pub use smol::channel::*;
}

pub mod spsc {
    pub use smol::channel::*;
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

use crate::{
    schedule::Borrow,
    storage::{CanReadStorage, CanWriteStorage, ParallelStorage},
};

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
    fn into_owned(self) -> Option<Resource> {
        match self {
            FetchReadyResource::Owned(r) => Some(r),
            FetchReadyResource::Ref(_) => None,
        }
    }

    fn into_ref(self) -> Option<Arc<Resource>> {
        match self {
            FetchReadyResource::Owned(_) => None,
            FetchReadyResource::Ref(r) => Some(r),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ResourceTypeId {
    Raw(TypeId),
    Storage(TypeId),
}

#[derive(Clone, Debug, Eq)]
pub struct ResourceId {
    pub(crate) type_id: TypeId,
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
            type_id: TypeId::of::<T>(), //ResourceTypeId::Raw(TypeId::of::<T>()),
            name: std::any::type_name::<T>(),
        }
    }

    //pub fn new_storage<T: IsComponent>() -> Self {
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

pub struct Write<T: IsResource> {
    fetched: Fetched<T>,
}

impl<'a, T: IsResource> Deref for Write<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.fetched.inner.as_ref().unwrap()
    }
}

impl<'a, T: IsResource> DerefMut for Write<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.fetched.inner.as_mut().unwrap()
    }
}

impl<T: IsResource + CanReadStorage> CanReadStorage for Write<T> {
    type Component = T::Component;

    type Iter<'a> = T::Iter<'a>
    where
        Self: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        self.fetched.get(id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.fetched.iter()
    }

    fn last(&self) -> Option<&crate::storage::Entry<Self::Component>> {
        self.fetched.last()
    }
}

impl<T: IsResource + CanWriteStorage> CanWriteStorage for Write<T> {
    type IterMut<'a> = T::IterMut<'a>
    where
        Self: 'a;

    fn get_mut(&mut self, id: usize) -> Option<&mut Self::Component> {
        self.fetched.get_mut(id)
    }

    fn insert(&mut self, id: usize, component: Self::Component) -> Option<Self::Component> {
        self.fetched.insert(id, component)
    }

    fn remove(&mut self, id: usize) -> Option<Self::Component> {
        self.fetched.remove(id)
    }

    fn iter_mut(&mut self) -> Self::IterMut<'_> {
        self.fetched.iter_mut()
    }
}

impl<S: IsResource + ParallelStorage> ParallelStorage for Write<S> {
    type ParIter<'a> = S::ParIter<'a>
    where
        Self: 'a;

    type IntoParIter<'a> = S::IntoParIter<'a>
    where
        Self: 'a;

    type ParIterMut<'a> = S::ParIterMut<'a>
    where
        Self: 'a;

    type IntoParIterMut<'a> = S::IntoParIterMut<'a>
    where
        Self: 'a;

    fn par_iter(&self) -> Self::IntoParIter<'_> {
        self.fetched.par_iter()
    }

    fn par_iter_mut(&mut self) -> Self::IntoParIterMut<'_> {
        self.fetched.par_iter_mut()
    }
}

pub struct Read<T: IsResource> {
    inner: Arc<Resource>,
    _phantom: PhantomData<T>,
}

impl<'a, T: IsResource> Deref for Read<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // I don't like this unwrap, but it works
        self.inner.downcast_ref().unwrap()
    }
}

impl<T: IsResource + CanReadStorage> CanReadStorage for Read<T> {
    type Component = T::Component;

    type Iter<'a> = T::Iter<'a>
    where
        Self: 'a;

    fn get(&self, id: usize) -> Option<&Self::Component> {
        self.deref().get(id)
    }

    fn iter(&self) -> Self::Iter<'_> {
        self.deref().iter()
    }

    fn last(&self) -> Option<&crate::storage::Entry<Self::Component>> {
        self.deref().last()
    }
}

#[derive(Debug, Default)]
pub struct Request {
    pub borrows: Vec<Borrow>,
}

pub trait CanFetch: Sized {
    fn reads() -> Vec<ResourceId>;
    fn writes() -> Vec<ResourceId>;
    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self>;
}

impl<'a, T: IsResource> CanFetch for Write<T> {
    fn writes() -> Vec<ResourceId> {
        vec![ResourceId::new::<T>()]
    }

    fn reads() -> Vec<ResourceId> {
        vec![]
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: FetchReadyResource = fields.remove(&id).with_context(|| {
            format!(
                "Write::construct could not find '{}' in resources",
                std::any::type_name::<T>(),
            )
        })?;
        let t = t.into_owned().context("resource is not owned")?;
        let inner: Option<Box<T>> = Some(t.downcast::<T>().map_err(|_| {
            anyhow::anyhow!(
                "Write::construct could not cast resource as '{}'",
                std::any::type_name::<T>(),
            )
        })?);
        let fetched = Fetched {
            resource_return_tx,
            inner,
        };
        Ok(Write { fetched })
    }
}

impl<'a, T: IsResource> CanFetch for Read<T> {
    fn writes() -> Vec<ResourceId> {
        vec![]
    }

    fn reads() -> Vec<ResourceId> {
        vec![ResourceId::new::<T>()]
    }

    fn construct(
        _: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut FxHashMap<ResourceId, FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: FetchReadyResource = fields.remove(&id).with_context(|| {
            format!(
                "Read::construct could not find '{}' in resources",
                std::any::type_name::<T>(),
            )
        })?;
        let inner = t.into_ref().context("resource is not borrowed")?;

        Ok(Read {
            inner,
            _phantom: PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbounded_channel_doesnt_yield_on_send_and_await() {
        let (tx, rx) = mpsc::unbounded::<u32>();

        let executor = smol::Executor::new();
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
    fn smol_executor_sanity() {
        let (tx, rx) = mpsc::bounded::<String>(1);

        let executor = smol::Executor::new();
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
