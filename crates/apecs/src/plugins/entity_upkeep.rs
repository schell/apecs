//! The entity upkeep plugin.
//!
//! Makes sure that destroyed entities have their components removed from
//! storages.
use std::marker::PhantomData;

use crate as apecs;
use crate::plugins::SyncSystemWithDeps;
use crate::{
    entities::Entities, plugins::LazyResource, storage::CanWriteStorage, world::World, CanFetch,
    Read, Write,
};

use super::IsPlugin;

pub struct DestroyedIds(Vec<usize>);

#[derive(CanFetch)]
pub struct PreEntityUpkeepData {
    entities: Write<Entities>,
    destroyed_ids: Write<DestroyedIds>,
}

fn pre_upkeep_system(mut data: PreEntityUpkeepData) -> anyhow::Result<()> {
    let dead = data
        .entities
        .dead
        .iter()
        .map(|e| e.id())
        .collect::<Vec<_>>();
    data.destroyed_ids.0 = dead;
    data.entities.recycle_dead();
    Ok(())
}

#[derive(CanFetch)]
pub struct EntityUpkeep<Storage: Send + Sync + 'static> {
    dead_ids: Read<DestroyedIds>,
    storage: Write<Storage>,
}

fn upkeep_system<Storage>(mut data: EntityUpkeep<Storage>) -> anyhow::Result<()>
where
    Storage: CanWriteStorage + Send + Sync + 'static,
{
    for id in data.dead_ids.0.iter() {
        let _ = data.storage.remove(*id);
    }
    Ok(())
}

/// The entity upkeep plugin.
pub struct PluginEntityUpkeep<Storage>(PhantomData<Storage>);

impl<Storage: CanWriteStorage + Default + Send + Sync + 'static> IsPlugin
    for PluginEntityUpkeep<Storage>
{
    fn resources(&self) -> Vec<super::LazyResource> {
        vec![
            LazyResource::new(|| Entities::default()),
            LazyResource::new(|| DestroyedIds(vec![])),
            LazyResource::new(|| Storage::default()),
        ]
    }

    fn sync_systems(&self) -> Vec<SyncSystemWithDeps> {
        vec![
            SyncSystemWithDeps::new("entity_pre_upkeep", pre_upkeep_system, None),
            SyncSystemWithDeps::new(
                &format!("entity_upkeep_{}", std::any::type_name::<Storage>()),
                upkeep_system::<Storage>,
                None,
            ),
        ]
    }
}

pub trait EntityUpkeepExt {
    fn with_storage<T>(&mut self, storage: T) -> anyhow::Result<&mut Self>
    where
        T: CanWriteStorage + Default + Send + Sync + 'static;

    fn with_default_storage<T>(&mut self) -> &mut Self
    where
        T: CanWriteStorage + Default + Send + Sync + 'static;
}

impl EntityUpkeepExt for World {
    fn with_storage<T>(&mut self, storage: T) -> anyhow::Result<&mut Self>
    where
        T: CanWriteStorage + Default + Send + Sync + 'static,
    {
        Ok(self
            .with_resource(storage)?
            .with_plugin(PluginEntityUpkeep::<T>(PhantomData)))
    }

    fn with_default_storage<T>(&mut self) -> &mut Self
    where
        T: CanWriteStorage + Default + Send + Sync + 'static,
    {
        self.with_plugin(PluginEntityUpkeep::<T>(PhantomData))
    }
}

#[cfg(test)]
mod test {
    use crate::{entities::Entities, storage::*, world::World, Write};

    use super::EntityUpkeepExt;

    #[test]
    fn can_run_storage_upkeep() {
        let mut world = World::default();
        world.with_default_storage::<VecStorage<usize>>();

        let c = {
            let (mut entities, mut abc): (Write<Entities>, Write<VecStorage<usize>>) =
                world.fetch().unwrap();

            let a = entities.create();
            let b = entities.create();
            let c = entities.create();
            for (ent, i) in vec![a, b, c].into_iter().zip(0..) {
                abc.insert(ent.id(), i);
            }

            c
        };

        world.tick().unwrap();

        {
            let abc: Write<VecStorage<usize>> = world.fetch().unwrap();
            assert_eq!(abc.get(c.id()), Some(&2));
        }

        {
            let mut entities: Write<Entities> = world.fetch().unwrap();
            entities.destroy(c);
        }

        world.tick().unwrap();

        {
            let abc: Write<VecStorage<usize>> = world.fetch().unwrap();
            assert_eq!(abc.get(c.id()), None);
        }
    }
}
