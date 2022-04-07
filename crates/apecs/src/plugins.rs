//! Plugins are collections of complimentary systems and resources.
use crate::{
    system::{AsyncSystem, SyncSystem},
    IsResource, Resource, ResourceId,
};

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

pub trait IsPlugin {
    /// Provide the resources required by this plugin
    fn resources(&self) -> Vec<LazyResource>;

    /// Provide the sync systems required by this plugin
    fn sync_systems(&self) -> Vec<SyncSystem>;

    /// Provide the async systems required by this plugin
    fn async_systems(&self) -> Vec<AsyncSystem> {
        todo!()
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
            for (_, s, n) in (&data.strings, &mut data.numbers).join() {
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

            fn sync_systems(&self) -> Vec<SyncSystem> {
                vec![SyncSystem::new("my_system", my_system)]
            }
        }

        let mut world = World::default();
        world.with_plugin(MyPlugin);
    }
}
