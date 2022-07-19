//! The entity upkeep plugin.
//!
//! Makes sure that destroyed entities have their components removed from
//! storages.
use crate::storage::separate::VecStorage;
use crate::system::{ok, ShouldContinue};
use crate::{self as apecs, WriteExpect};
use crate::{world::Entities, CanFetch, Read, Write};

use super::Plugin;

#[derive(Default)]
pub struct DestroyedIds(Vec<usize>);

#[derive(CanFetch)]
pub struct PreEntityUpkeepData {
    entities: WriteExpect<Entities>,
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
pub struct EntityUpkeep<T: Send + Sync + 'static> {
    dead_ids: Read<DestroyedIds>,
    storage: Write<VecStorage<T>>,
}

fn upkeep_system<T: Send + Sync + 'static>(
    mut data: EntityUpkeep<T>,
) -> anyhow::Result<ShouldContinue>
where
{
    data.storage.upkeep(&data.dead_ids.0);
    ok()
}

/// The upkeep plugin for a component storage
pub fn plugin<T: Send + Sync + 'static>() -> Plugin {
    Plugin::default()
        .with_system("entity_pre_upkeep", pre_upkeep_system, &[])
        .with_system(
            &format!("entity_upkeep_{}", std::any::type_name::<VecStorage<T>>()),
            upkeep_system::<T>,
            &["entity_pre_upkeep"],
        )
}

#[cfg(test)]
mod test {
    use crate::{storage::separate::*, world::Entities, world::World, Write};

    #[test]
    fn can_run_storage_upkeep() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world.with_default_storage::<usize>().unwrap();

        let c = {
            let (mut entities, mut abc): (Write<Entities>, WriteStore<usize>) =
                world.fetch().unwrap();

            let a = entities.create();
            let b = entities.create();
            let c = entities.create();
            for (ent, i) in vec![a, b, c.clone()].into_iter().zip(0..) {
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
            entities.destroy(c.clone());
        }

        world.tick().unwrap();

        {
            let abc: Write<VecStorage<usize>> = world.fetch().unwrap();
            assert_eq!(abc.get(c.id()), None);
        }
    }
}
