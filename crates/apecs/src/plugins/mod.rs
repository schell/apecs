//! Plugins are collections of complimentary systems and resources.
use crate::{
    system::{SyncSystem, AsyncSystemFuture},
    IsResource, Resource, ResourceId, world::Facade, CanFetch,
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
    pub fn new<T, F>(name: &str, f:F, deps: Option<&[&str]>) -> Self
        where
        F: FnMut(T) -> anyhow::Result<()> + Send + Sync + 'static,
        T: CanFetch,
    {
        let mut vs = vec![];
        if let Some(names) = deps {
            for name in names {
                vs.push(name.to_string());
            }
        }
        SyncSystemWithDeps(SyncSystem::new(name, f), vs)
    }
}

pub struct LazyAsyncSystem(pub String, pub Box<dyn FnOnce(Facade) -> AsyncSystemFuture>);

pub trait IsPlugin {
    /// Provide the resources required by this plugin
    fn resources(&self) -> Vec<LazyResource> {
        vec![]
    }

    /// Provide the sync systems and their dependencies required by this plugin
    fn sync_systems(&self) -> Vec<SyncSystemWithDeps> {
        vec![]
    }

    /// Provide the async systems required by this plugin
    fn async_systems(&self) -> Vec<LazyAsyncSystem> {
        vec![]
    }
}

impl<A, B> IsPlugin for (A, B)
where
    A: IsPlugin,
    B: IsPlugin,
{
    fn resources(&self) -> Vec<LazyResource> {
        let mut r = vec![];
        r.extend(self.0.resources());
        r.extend(self.1.resources());
        r
    }

    fn sync_systems(&self) -> Vec<SyncSystemWithDeps> {
        let mut r = vec![];
        r.extend(self.0.sync_systems());
        r.extend(self.1.sync_systems());
        r
    }

    fn async_systems(&self) -> Vec<LazyAsyncSystem> {
        let mut r = vec![];
        r.extend(self.0.async_systems());
        r.extend(self.1.async_systems());
        r
    }
}

#[cfg(test)]
mod test {
    use crate as apecs;

    use super::*;

    #[test]
    fn sanity() {
        use apecs::{storage::VecStorage, Write, join::Join, world::World, CanFetch};
        struct MyPlugin;

        #[derive(CanFetch)]
        struct MyData {
            strings: Write<VecStorage<&'static str>>,
            numbers: Write<VecStorage<usize>>,
        }

        fn my_system(mut data: MyData) -> anyhow::Result<()> {
            for (_, _, n) in (&data.strings, &mut data.numbers).join() {
                *n += 1;
            }

            Ok(())
        }

        impl IsPlugin for MyPlugin {
            fn resources(&self) -> Vec<LazyResource> {
                vec![
                    LazyResource::new(|| VecStorage::<&'static str>::default()),
                    LazyResource::new(|| VecStorage::<usize>::default()),
                ]
            }

            fn sync_systems(&self) -> Vec<SyncSystemWithDeps> {
                vec![SyncSystemWithDeps::new("my_system", my_system, None)]
            }
        }

        let mut world = World::default();
        world.with_plugin(MyPlugin);
    }
}
