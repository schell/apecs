//! Plugins are collections of complimentary systems and resources.
use std::future::Future;

use crate::{
    system::{AsyncSystemFuture, ShouldContinue, SyncSystem},
    world::Facade,
    CanFetch, IsResource, Resource, ResourceId,
};

pub mod entity_upkeep;

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

pub struct SyncSystemWithDeps(pub SyncSystem, pub Vec<String>);

impl SyncSystemWithDeps {
    pub fn new<T, F>(name: &str, system: F, deps: Option<&[&str]>) -> Self
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch,
    {
        let mut vs = vec![];
        if let Some(names) = deps {
            for name in names {
                vs.push(name.to_string());
            }
        }
        SyncSystemWithDeps(SyncSystem::new(name, system), vs)
    }
}

pub struct LazyAsyncSystem(pub String, pub Box<dyn FnOnce(Facade) -> AsyncSystemFuture>);

impl<A, B> From<(A, B)> for Plugin
where
    A: Into<Plugin>,
    B: Into<Plugin>,
{
    fn from((a, b): (A, B)) -> Self {
        a.into().with_plugin(b.into())
    }
}

#[derive(Default)]
pub struct Plugin {
    pub resources: Vec<LazyResource>,
    pub sync_systems: Vec<SyncSystemWithDeps>,
    pub async_systems: Vec<LazyAsyncSystem>,
}

impl Plugin {
    pub fn with_plugin(mut self, plug: impl Into<Plugin>) -> Self {
        let plug = plug.into();
        self.resources.extend(plug.resources);
        self.sync_systems.extend(plug.sync_systems);
        self.async_systems.extend(plug.async_systems);
        self
    }

    pub fn with_resource<T: IsResource>(mut self, mk_rez: impl FnOnce() -> T + 'static) -> Self {
        self.resources.push(LazyResource::new(mk_rez));
        self
    }

    pub fn with_system<T: CanFetch>(
        mut self,
        name: &str,
        system: impl FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        deps: &[&str],
    ) -> Self {
        let deps = if deps.is_empty() { None } else { Some(deps) };
        self.sync_systems
            .push(SyncSystemWithDeps::new(name, system, deps));
        self
    }

    pub fn with_async_system<Fut>(
        mut self,
        name: &str,
        system: impl FnOnce(Facade) -> Fut + 'static,
    ) -> Self
    where
        Fut: Future<Output = anyhow::Result<()>> + Send + Sync + 'static,
    {
        self.async_systems.push(LazyAsyncSystem(
            name.to_string(),
            Box::new(move |facade| Box::pin(system(facade))),
        ));
        self
    }
}

#[cfg(test)]
mod test {
    use crate as apecs;

    use super::*;

    #[test]
    fn sanity() {
        use apecs::{join::Join, storage::VecStorage, system::*, world::World, CanFetch, Write};
        struct MyPlugin;

        #[derive(CanFetch)]
        struct MyData {
            strings: Write<VecStorage<&'static str>>,
            numbers: Write<VecStorage<usize>>,
        }

        fn my_system(mut data: MyData) -> anyhow::Result<ShouldContinue> {
            for (_, _, n) in (&data.strings, &mut data.numbers).join() {
                *n += 1;
            }

            ok()
        }

        impl From<MyPlugin> for Plugin {
            fn from(_: MyPlugin) -> Plugin {
                Plugin::default()
                    .with_resource(|| VecStorage::<&'static str>::default())
                    .with_resource(|| VecStorage::<usize>::default())
                    .with_system("my_system", my_system, &[])
            }
        }

        let mut world = World::default();
        world.with_plugin(MyPlugin);
    }
}
