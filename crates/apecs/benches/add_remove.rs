use std::marker::PhantomData;

use apecs::{storage::*, world::*, Write};

// TODO: Remove clone constraint
#[derive(Clone)]
pub struct A(f32);

#[derive(Clone)]
pub struct B(f32);

pub struct Benchmark<Store1, Store2> {
    world: World,
    entities: Vec<Entity>,
    _phantom: PhantomData<(Store1, Store2)>,
}

impl<S1, S2> Benchmark<S1, S2>
where
    S1: WorldStorage<Component = A>,
    S2: WorldStorage<Component = B>,
{
    pub fn new() -> Self {
        let mut world = World::default();
        world
            .with_resource(S1::default())
            .unwrap()
            .with_resource(S2::default())
            .unwrap();

        let entities = {
            let (mut entities, mut a_storage): (Write<Entities>, Write<S1>) =
                world.fetch().unwrap();
            (0..10000)
                .map(|_| {
                    let e = entities.create();
                    let _ = a_storage.insert(e.id(), A(0.0));
                    e
                })
                .collect()
        };

        Self {
            world,
            entities,
            _phantom: PhantomData,
        }
    }

    pub fn run(&mut self) {
        let mut b_storage: Write<S2> = self.world.fetch().unwrap();
        for entity in &self.entities {
            let _ = b_storage.insert(entity.id(), B(0.0));
        }

        for entity in &self.entities {
            b_storage.remove(entity.id());
        }
    }
}
