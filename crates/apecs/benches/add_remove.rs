use apecs::{
    storage::{archetype::*, separated::*, Entry},
    world::*,
    Write,
};

pub struct A(f32);
pub struct B(f32);

pub struct BenchmarkSeparate {
    world: World,
    entities: Vec<Entity>,
}

impl BenchmarkSeparate {
    pub fn new() -> Self {
        let mut world = World::default();
        world
            .with_resource(VecStorage::<A>::new_with_capacity(10001))
            .unwrap()
            .with_resource(VecStorage::<B>::new_with_capacity(10001))
            .unwrap();

        let entities = {
            let (mut entities, mut a_storage): (Write<Entities>, WriteStore<A>) =
                world.fetch().unwrap();
            (0..10000)
                .map(|_| {
                    let e = entities.create();
                    let _ = a_storage.insert(e.id(), A(0.0));
                    e
                })
                .collect()
        };

        Self { world, entities }
    }

    pub fn run(&mut self) {
        let mut b_storage: WriteStore<B> = self.world.fetch().unwrap();
        for entity in &self.entities {
            let _ = b_storage.insert(entity.id(), B(0.0));
        }

        for entity in &self.entities {
            b_storage.remove(entity.id());
        }
    }
}

pub struct BenchmarkArchetype(AllArchetypes);

impl BenchmarkArchetype {
    pub fn new() -> Self {
        let mut all = AllArchetypes::default();
        all.extend::<(A,)>(Box::new((0..10_000).map(|id| Entry::new(id, A(0.0)))));
        Self(all)
    }

    pub fn run(&mut self) {
        for id in 0..10000 {
            let _ = self.0.insert_component(id, B(0.0));
        }

        for id in 0..10000 {
            let _ = self.0.remove_component::<B>(id);
        }
    }
}
