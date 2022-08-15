//! The entity upkeep plugin.
//!
//! Makes sure that destroyed entities have their components removed from
//! storages.
use crate::storage::ArchetypeSet;
#[cfg(feature = "storage-archetype")]
use crate::storage::archetype::ArchetypeSet;
#[cfg(feature = "storage-separated")]
use crate::storage::separated::VecStorage;
use crate::system::{ok, ShouldContinue};
use crate::{self as apecs, WriteExpect, Read};
use crate::{world::Entities, CanFetch, Write};

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

fn archetype_upkeep_system(
    mut data: (Read<DestroyedIds>, Write<ArchetypeSet>)
) -> anyhow::Result<ShouldContinue>
where
{
    data.1.upkeep(&data.0.0);
    ok()
}

/// The upkeep plugin for archetype component storage
pub fn plugin_storage_archetype_plugin() -> Plugin {
    Plugin::default()
        .with_default_resource::<ArchetypeSet>()
        .with_system("entity_pre_upkeep", pre_upkeep_system, &[])
        .with_system("entity_upkeep_archetype", archetype_upkeep_system, &["entity_pre_upkeep"])
}

#[cfg(test)]
mod test {
    use crate::{world::Entities, world::World, Write};

    #[test]
    fn can_run_archetype_storage_upkeep() {
        use crate::{storage::*, Read, plugins::entity_upkeep::plugin_storage_archetype_plugin};
        use std::ops::Deref;

        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let mut world = World::default();
        world
            .with_plugin(plugin_storage_archetype_plugin())
            .unwrap();

        let c = {
            let (mut entities, mut all): (Write<Entities>, Write<ArchetypeSet>) =
                world.fetch().unwrap();

            let a = entities.create();
            let b = entities.create();
            let c = entities.create();
            for (ent, i) in vec![a, b, c.clone()].into_iter().zip(0usize..) {
                all.insert_component(ent.id(), i);
            }

            c
        };

        world.tick();

        {
            let (q, all): (Query<&usize>, Read<ArchetypeSet>) = world.fetch().unwrap();
            assert_eq!(Some(2), q.query().find_one(c.id()).map(|i| **i), "{:#?}", all.deref());
        }

        {
            let mut entities: Write<Entities> = world.fetch().unwrap();
            entities.destroy(c.clone());
        }

        world.tick();

        {
            let (q, all): (Query<&usize>, Read<ArchetypeSet>) = world.fetch().unwrap();
            assert_eq!(None, q.query().find_one(c.id()), "{:#?}", all.deref());
        }
    }
}
