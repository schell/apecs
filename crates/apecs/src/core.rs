//! Core types and processes
//!
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use anyhow::Context;

pub use apecs_derive::CanFetch;

pub mod mpsc {
    pub use smol::channel::*;
}

pub mod spsc {
    pub use smol::channel::*;
}

mod fetch;
pub use fetch::*;

pub trait IsResource: Any + Send + Sync + 'static {}
impl<T: Any + Send + Sync + 'static> IsResource for T {}

pub type Resource = Box<dyn Any + Send + Sync + 'static>;

#[derive(Clone, Debug, Eq)]
pub struct ResourceId {
    pub(crate) type_id: TypeId,
    pub(crate) name: String,
}

impl std::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name.as_str())
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
            name: std::any::type_name::<T>().to_string(),
        }
    }
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
        &self.inner.as_ref().unwrap()
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

pub struct Read<T: IsResource> {
    fetched: Fetched<T>,
}

impl<'a, T: IsResource> Deref for Read<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.fetched.inner.as_ref().unwrap()
    }
}

pub(crate) struct Request {
    pub resource_ids: Vec<ResourceId>,
}

pub struct Facade {
    // Unbounded. Sending from the system should not yield the async
    pub(crate) resource_request_tx: spsc::Sender<Request>,
    // Bounded(1). Awaiting in the system should yield the async
    pub(crate) resources_to_system_rx: spsc::Receiver<HashMap<ResourceId, Resource>>,
    // Unbounded. Sending from the system should not yield the async
    pub(crate) resources_from_system_tx: mpsc::Sender<(ResourceId, Resource)>,
}

impl Facade {
    pub async fn fetch<'a, T: CanFetch>(&'a mut self) -> anyhow::Result<T> {
        let reads = T::reads();
        let writes = T::writes();
        let mut resource_ids = reads;
        resource_ids.extend(writes);
        self.resource_request_tx
            .try_send(Request { resource_ids })
            .context("could not send request for resources")?;

        let mut resources = self
            .resources_to_system_rx
            .recv()
            .await
            .context("could not fetch resources")?;
        T::construct(self.resources_from_system_tx.clone(), &mut resources)
            .context("could not construct system data")
    }
}

pub trait CanFetch: Sized {
    fn reads() -> Vec<ResourceId>;
    fn writes() -> Vec<ResourceId>;
    fn construct<'a>(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut HashMap<ResourceId, Resource>,
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
        fields: &mut HashMap<ResourceId, Resource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: Resource = fields.remove(&id).context(format!(
            "could not find '{}' in resources",
            std::any::type_name::<T>(),
        ))?;
        let inner: Option<Box<T>> = Some(t.downcast::<T>().map_err(|_| {
            anyhow::anyhow!(
                "could not cast resource as '{}'",
                std::any::type_name::<T>(),
            )
        })?);
        let fetched = Fetched {
            resource_return_tx,
            inner,
        };
        Ok(Write { fetched })
    }

    //fn deconstruct(self) -> HashMap<ResourceId, Resource> {
    //    HashMap::from([(ResourceId::new::<T>(), self.inner as Resource)])
    //}
}

impl<'a, T: IsResource> CanFetch for Read<T> {
    fn writes() -> Vec<ResourceId> {
        vec![]
    }

    fn reads() -> Vec<ResourceId> {
        vec![ResourceId::new::<T>()]
    }

    fn construct(
        resource_return_tx: mpsc::Sender<(ResourceId, Resource)>,
        fields: &mut HashMap<ResourceId, Resource>,
    ) -> anyhow::Result<Self> {
        let id = ResourceId::new::<T>();
        let t: Resource = fields.remove(&id).context(format!(
            "could not find '{}' in resources",
            std::any::type_name::<T>(),
        ))?;
        let inner: Option<Box<T>> = Some(t.downcast().map_err(|_| {
            anyhow::anyhow!(
                "could not cast resource as '{}'",
                std::any::type_name::<T>(),
            )
        })?);
        let fetched = Fetched {
            resource_return_tx,
            inner,
        };
        Ok(Read { fetched })
    }

    //fn deconstruct(self) -> HashMap<ResourceId, Resource> {
    //    HashMap::from([(ResourceId::new::<T>(), self.inner as Resource)])
    //}
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
        while let Some(msg) = rx.try_recv().ok() {
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

        let tx_s = tx.clone();
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
