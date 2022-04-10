//! Entity creation.

use std::{iter::Map, ops::Deref};

use hibitset::{BitIter, BitSet, BitSetLike};

use crate::storage::Entry;

#[derive(Clone, Copy)]
pub struct Entity {
    id: usize,
    gen: usize,
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
    pub next_k: usize,
    pub alive_set: BitSet,
    pub dead: Vec<Entity>,
    pub recycle: Vec<Entity>,
}

impl Entities {
    pub fn new() -> Self {
        Entities {
            next_k: 0,
            alive_set: BitSet::new(),
            dead: vec![],
            recycle: vec![],
        }
    }

    pub fn create(&mut self) -> Entity {
        let entity = if self.recycle.is_empty() {
            let id = self.next_k;
            self.next_k += 1;
            Entity { id, gen: 0 }
        } else {
            self.recycle.pop().unwrap()
        };

        self.alive_set.add(entity.id.try_into().unwrap());
        entity
    }

    pub fn destroy(&mut self, entity: Entity) {
        self.alive_set.remove(entity.id.try_into().unwrap());
        self.dead.push(entity);
    }

    pub fn iter(&self) -> Map<BitIter<&BitSet>, fn(u32) -> Entry<usize>> {
        (&self.alive_set).iter().map(|id| Entry {
            key: id as usize,
            value: id as usize,
        })
    }

    pub fn recycle_dead(&mut self) {
        for mut dead in std::mem::take(&mut self.dead).into_iter() {
            dead.gen += 1;
            self.recycle.push(dead);
        }
    }
}
