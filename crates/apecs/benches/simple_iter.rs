use apecs::{anyhow, storage::{separated::*, archetype::*}};
use cgmath::*;

#[derive(Copy, Clone)]
pub struct Transform(Matrix4<f32>);

#[derive(Copy, Clone)]
pub struct Position(Vector3<f32>);

#[derive(Copy, Clone)]
pub struct Rotation(Vector3<f32>);

#[derive(Copy, Clone)]
pub struct Velocity(Vector3<f32>);

pub struct BenchmarkSeparate {
    ps: VecStorage<Position>,
    vs: VecStorage<Velocity>,
}

impl BenchmarkSeparate {
    pub fn new() -> anyhow::Result<Self> {
        let mut ps = VecStorage::<Position>::new_with_capacity(10001);
        let mut vs = VecStorage::<Velocity>::new_with_capacity(10001);

        (0..10000).for_each(|id| {
            ps.insert(id, Position(Vector3::unit_x()));
            vs.insert(id, Velocity(Vector3::unit_x()));
        });

        Ok(Self { ps, vs })
    }

    pub fn run(&mut self) {
        for (velocity, position) in (&self.vs, &mut self.ps).join() {
            position.0 += velocity.0;
        }
    }
}

pub struct BenchmarkArchetype(AllArchetypes);

impl BenchmarkArchetype {
    pub fn new() -> anyhow::Result<Self> {
        let mut archs = AllArchetypes::default();

        (0..10000).for_each(|id| {
            let _ = archs.insert_bundle(id, (Position(Vector3::unit_x()), Velocity(Vector3::unit_x())));
        });

        Ok(Self (archs))
    }

    pub fn run(&mut self) {
        let mut query: Query<(&Velocity, &mut Position)> = Query::try_from(&mut self.0).unwrap();
        for (velocity, position) in query.iter_mut() {
            position.0 += velocity.0;
        }
    }
}
