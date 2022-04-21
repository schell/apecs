//! The entity upkeep plugin.
//!
//! Makes sure that destroyed entities have their components removed from
//! storages.
use std::marker::PhantomData;

use crate as apecs;
use crate::storage::{WorldStorage, Tracker};
use crate::system::{ok, ShouldContinue};
use crate::{entities::Entities, CanFetch, Read, Write};

use super::Plugin;

pub struct DestroyedIds(Vec<usize>);

#[derive(CanFetch)]
pub struct PreEntityUpkeepData {
    entities: Write<Entities>,
    destroyed_ids: Write<DestroyedIds>,
}

fn pre_upkeep_system(mut data: PreEntityUpkeepData) -> anyhow::Result<ShouldContinue> {
    let dead = data
        .entities
        .dead
        .iter()
        .map(|e| e.id())
        .collect::<Vec<_>>();
    data.destroyed_ids.0 = dead;
    data.entities.recycle_dead();
    ok()
}

#[derive(CanFetch)]
pub struct EntityUpkeep<Storage: Send + Sync + 'static> {
    dead_ids: Read<DestroyedIds>,
    storage: Write<Storage>,
}

fn upkeep_system<Storage>(mut data: EntityUpkeep<Storage>) -> anyhow::Result<ShouldContinue>
where
    Storage: WorldStorage,
{
    data.storage.upkeep(&data.dead_ids.0);
    ok()
}

/// The entity upkeep plugin.
pub struct PluginEntityUpkeep<Storage>(pub(crate) PhantomData<Storage>);

impl<Storage: WorldStorage> From<PluginEntityUpkeep<Storage>> for Plugin {
    fn from(_: PluginEntityUpkeep<Storage>) -> Self {
        Plugin::default()
            .with_resource(|| Entities::default())
            .with_resource(|| DestroyedIds(vec![]))
            .with_resource(|| Storage::default())
            .with_resource(|| Tracker::<Storage>::default())
            .with_system("entity_pre_upkeep", pre_upkeep_system, &[])
            .with_system(
                &format!("entity_upkeep_{}", std::any::type_name::<Storage>()),
                upkeep_system::<Storage>,
                &["entity_pre_upkeep"],
            )
    }
}

#[cfg(test)]
mod test {
    use crate::{entities::Entities, storage::*, world::World, Write};

    #[test]
    fn can_run_storage_upkeep() {
        impl StoredComponent for usize {
            type StorageType = VecStorage<Self>;
        }

        let mut world = World::default();
        world.with_default_storage::<usize>().unwrap();

        let c = {
            let (mut entities, mut abc): (Write<Entities>, WriteStore<usize>) =
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
