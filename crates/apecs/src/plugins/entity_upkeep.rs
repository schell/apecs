//! The entity upkeep plugin.
//!
//! Makes sure that destroyed entities have their components removed from
//! storages.
#[cfg(feature = "storage-archetype")]
use crate::storage::archetype::AllArchetypes;
#[cfg(feature = "storage-separated")]
use crate::storage::separated::VecStorage;
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

#[cfg(feature = "storage-separated")]
#[derive(CanFetch)]
pub struct SeparatedEntityUpkeep<T: Send + Sync + 'static> {
    dead_ids: Read<DestroyedIds>,
    storage: Write<VecStorage<T>>,
}

#[cfg(feature = "storage-separated")]
fn separated_upkeep_system<T: Send + Sync + 'static>(
    mut data: SeparatedEntityUpkeep<T>,
) -> anyhow::Result<ShouldContinue>
where
{
    data.storage.upkeep(&data.dead_ids.0);
    ok()
}

#[cfg(feature = "storage-separated")]
/// The upkeep plugin for a separated component storage
pub fn plugin_storage_separated_upkeep<T: Send + Sync + 'static>() -> Plugin {
    Plugin::default()
        .with_system("entity_pre_upkeep", pre_upkeep_system, &[])
        .with_system(
            &format!("entity_upkeep_{}", std::any::type_name::<VecStorage<T>>()),
            separated_upkeep_system::<T>,
            &["entity_pre_upkeep"],
        )
}

#[cfg(feature = "storage-archetype")]
fn archetype_upkeep_system(
    mut data: (Read<DestroyedIds>, Write<AllArchetypes>)
) -> anyhow::Result<ShouldContinue>
where
{
    data.1.upkeep(&data.0.0);
    ok()
}

#[cfg(feature = "storage-archetype")]
/// The upkeep plugin for archetype component storage
pub fn plugin_storage_archetype_plugin() -> Plugin {
    Plugin::default()
        .with_system("entity_pre_upkeep", pre_upkeep_system, &[])
        .with_system("entity_upkeep_archetype", archetype_upkeep_system, &["entity_pre_upkeep"])
}

#[cfg(test)]
mod test {
    use crate::{world::Entities, world::World, Write};

    #[cfg(feature = "storage-separated")]
    #[test]
    fn can_run_separated_storage_upkeep() {
        use crate::storage::separated::*;

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

    #[cfg(feature = "storage-archetype")]
    #[test]
    fn can_run_archetype_storage_upkeep() {
        use crate::{storage::archetype::*, Read, plugins::entity_upkeep::plugin_storage_archetype_plugin};
        use std::ops::Deref;

        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world
            .with_default_resource::<AllArchetypes>().unwrap()
            .with_plugin(plugin_storage_archetype_plugin())
            .unwrap();

        let c = {
            let (mut entities, mut all): (Write<Entities>, Write<AllArchetypes>) =
                world.fetch().unwrap();

            let a = entities.create();
            let b = entities.create();
            let c = entities.create();
            for (ent, i) in vec![a, b, c.clone()].into_iter().zip(0usize..) {
                all.insert_component(ent.id(), i);
            }

            c
        };

        world.tick().unwrap();

        {
            let (q, all): (Query<&usize>, Read<AllArchetypes>) = world.fetch().unwrap();
            let mut lock = q.lock();
            assert_eq!(Some(2), lock.find_one(c.id()).map(|i| **i), "{:#?}", all.deref());
        }

        {
            let mut entities: Write<Entities> = world.fetch().unwrap();
            entities.destroy(c.clone());
        }

        world.tick().unwrap();

        {
            let (q, all): (Query<&usize>, Read<AllArchetypes>) = world.fetch().unwrap();
            let mut lock = q.lock();
            assert_eq!(None, lock.find_one(c.id()), "{:#?}", all.deref());
        }
    }
}
