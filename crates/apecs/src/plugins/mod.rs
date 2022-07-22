//! Plugins are collections of complimentary systems and resources.
use std::future::Future;

use crate::{
    system::{AsyncSystemFuture, ShouldContinue, SyncSystem},
    world::Facade,
    CanFetch, IsResource, LazyResource, ResourceId, ResourceRequirement,
};

pub mod entity_upkeep;

pub struct SyncSystemWithDeps(pub SyncSystem, pub Vec<String>);

impl SyncSystemWithDeps {
    pub fn new<T, F>(name: &str, system: F, deps: Option<&[&str]>) -> Self
    where
        F: FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        T: CanFetch + Send + Sync + 'static,
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
    pub resources: Vec<ResourceRequirement>,
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

    /// Add a dependency on a resource that can be created with
    /// [`Default::default()`].
    ///
    /// If this resource does not already exist in the world at the time this
    /// plugin is instantiated, it will be inserted into the [`World`].
    pub fn with_default_resource<T: IsResource + Default>(mut self) -> Self {
        self.resources
            .push(ResourceRequirement::LazyDefault(LazyResource::new(|| {
                T::default()
            })));
        self
    }

    /// Add a dependency on a resource that can be created lazily with a
    /// closure.
    ///
    /// If a resource of this type does not already exist in the world at the
    /// time the plugin is instantiated, it will be inserted into the
    /// [`World`].
    pub fn with_lazy_resource<T: IsResource>(
        mut self,
        create: impl FnOnce() -> T + 'static,
    ) -> Self {
        self.resources
            .push(ResourceRequirement::LazyDefault(LazyResource::new(create)));
        self
    }

    /// Add a dependency on a resource that must already exist in the [`World`]
    /// at the time of plugin instantiation.
    ///
    /// If this resource does not already exist in the world at the time this
    /// plugin is instantiated, adding the plugin will err.
    pub fn with_expected_resource<T: IsResource>(mut self) -> Self {
        self.resources
            .push(ResourceRequirement::ExpectedExisting(ResourceId::new::<T>()));
        self
    }

    pub fn with_system<T: CanFetch + Send + Sync + 'static>(
        mut self,
        name: &str,
        system: impl FnMut(T) -> anyhow::Result<ShouldContinue> + Send + Sync + 'static,
        deps: &[&str],
    ) -> Self {
        let deps = if deps.is_empty() { None } else { Some(deps) };
        self.sync_systems
            .push(SyncSystemWithDeps::new(name, system, deps));
        self.with_plugin(T::plugin())
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
    use apecs::{system::*, storage::separate::*, world::World, CanFetch, Write};

    #[test]
     fn sanity() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        #[derive(CanFetch)]
        struct MyData {
            strings: Write<VecStorage<&'static str>>,
            numbers: Write<VecStorage<usize>>,
        }

        fn my_system(mut data: MyData) -> anyhow::Result<ShouldContinue> {
            for (_, n) in (&data.strings, &mut data.numbers).join() {
                n.set_value(n.value() + 1);
            }

            ok()
        }

        let plugin = MyData::plugin();
        assert_eq!(2, plugin.resources.len());

        let mut world = World::default();
        world.with_system("my_system", my_system).unwrap();

        let _data = world.fetch::<MyData>().unwrap();
    }
}
