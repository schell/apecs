//! Entity creation.

use std::{iter::Map, ops::Deref};

use hibitset::{BitIter, BitSet, BitSetLike};

use crate::storage::Entry;

pub struct Entity {
    id: usize,
}

impl Deref for Entity {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.id
    }
}

impl Entity {
    pub fn id(&self) -> usize {
        self.id
    }
}

#[derive(Default)]
pub struct Entities {
    pub(crate) next_k: usize,
    pub(crate) alive_set: BitSet,
}

impl Entities {
    pub fn new() -> Self {
        Entities {
            next_k: 0,
            alive_set: BitSet::new(),
        }
    }

    pub fn create(&mut self) -> Entity {
        let id = self.next_k;
        self.next_k += 1;
        self.alive_set.add(id.try_into().unwrap());
        Entity { id }
    }

    pub fn destroy(&mut self, entity: Entity) {
        self.alive_set.remove(entity.id.try_into().unwrap());
    }

    pub fn iter(&self) -> Map<BitIter<&BitSet>, fn(u32) -> Entry<Entity>> {
        (&self.alive_set).iter().map(|id| Entry {
            key: id as usize,
            value: Entity { id: id as usize },
        })
    }
}
