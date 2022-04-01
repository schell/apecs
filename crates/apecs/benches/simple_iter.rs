use std::marker::PhantomData;

use apecs::{
    anyhow,
    entities::*,
    join::Join,
    storage::{CanReadStorage, CanWriteStorage, WorldStorage},
    world::World,
    CanFetch, Read, Write,
};
use cgmath::*;

#[derive(Copy, Clone)]
pub struct Transform(Matrix4<f32>);

#[derive(Copy, Clone)]
pub struct Position(Vector3<f32>);

#[derive(Copy, Clone)]
pub struct Rotation(Vector3<f32>);

#[derive(Copy, Clone)]
pub struct Velocity(Vector3<f32>);

#[derive(CanFetch)]
struct Data<S1: WorldStorage, S2: WorldStorage, S3: WorldStorage, S4: WorldStorage> {
    es: Write<Entities>,
    ts: Write<S1>,
    ps: Write<S2>,
    rs: Write<S3>,
    vs: Write<S4>,
}

#[derive(CanFetch)]
struct SimpleIterData<S2: WorldStorage, S4: WorldStorage>
where
    S2: CanReadStorage<Component = Position>,
    S4: CanReadStorage<Component = Velocity>,
{
    ps: Write<S2>,
    vs: Read<S4>,
}

fn sync_run<S2: WorldStorage, S4: WorldStorage>(
    SimpleIterData { vs, mut ps }: SimpleIterData<S2, S4>,
) -> anyhow::Result<()>
where
    S2: CanReadStorage<Component = Position>,
    S4: CanReadStorage<Component = Velocity>,
{
    for (_, velocity, position) in (&vs, &mut ps).join() {
        position.0 += velocity.0;
    }

    Ok(())
}

pub struct Benchmark<S1: WorldStorage, S2: WorldStorage, S3: WorldStorage, S4: WorldStorage>
where
    S1: CanReadStorage<Component = Transform>,
    S2: CanReadStorage<Component = Position>,
    S3: CanReadStorage<Component = Rotation>,
    S4: CanReadStorage<Component = Velocity>,
{
    world: World,
    _phantom: PhantomData<(S1, S2, S3, S4)>,
}

impl<S1: WorldStorage, S2: WorldStorage, S3: WorldStorage, S4: WorldStorage>
    Benchmark<S1, S2, S3, S4>
where
    S1: CanReadStorage<Component = Transform>,
    S2: CanReadStorage<Component = Position>,
    S3: CanReadStorage<Component = Rotation>,
    S4: CanReadStorage<Component = Velocity>,
{
    pub fn new() -> anyhow::Result<Self> {
        let mut world = World::default();
        world
            .with_default_resource::<Entities>()?
            .with_resource(S1::new_with_capacity(10000))?
            .with_resource(S2::new_with_capacity(10000))?
            .with_resource(S3::new_with_capacity(10000))?
            .with_resource(S4::new_with_capacity(10000))?
            .with_system("simple_iter", sync_run::<S2, S4>);

        {
            let Data {
                mut es,
                mut ts,
                mut ps,
                mut rs,
                mut vs,
            } = world.fetch::<Data<S1, S2, S3, S4>>().unwrap();

            (0..10000).for_each(|_| {
                let e = es.create();
                let id = e.id();
                ts.insert(id, Transform(Matrix4::<f32>::from_scale(1.0)));
                ps.insert(id, Position(Vector3::unit_x()));
                rs.insert(id, Rotation(Vector3::unit_x()));
                vs.insert(id, Velocity(Vector3::unit_x()));
            });
        }

        Ok(Self {
            world,
            _phantom: PhantomData,
        })
    }

    pub fn run(&mut self) {
        self.world.tick_with_context(None).unwrap()
    }
}
