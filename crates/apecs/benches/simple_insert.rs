use apecs::storage::{archetype::AllArchetypes, separated::*};
use cgmath::*;

pub struct Transform(Matrix4<f32>);
pub struct Position(Vector3<f32>);
pub struct Rotation(Vector3<f32>);
pub struct Velocity(Vector3<f32>);

pub struct BenchmarkSeparate {
    ts: VecStorage<Transform>,
    ps: VecStorage<Position>,
    rs: VecStorage<Rotation>,
    vs: VecStorage<Velocity>,
}

impl BenchmarkSeparate {
    pub fn new() -> Self {
        BenchmarkSeparate {
            ts: VecStorage::<Transform>::new_with_capacity(10000),
            ps: VecStorage::<Position>::new_with_capacity(10000),
            rs: VecStorage::<Rotation>::new_with_capacity(10000),
            vs: VecStorage::<Velocity>::new_with_capacity(10000),
        }
    }

    pub fn run(&mut self) {
        (0..10000).for_each(|id| {
            self.ts
                .insert(id, Transform(Matrix4::<f32>::from_scale(1.0)));
            self.ps.insert(id, Position(Vector3::unit_x()));
            self.rs.insert(id, Rotation(Vector3::unit_x()));
            self.vs.insert(id, Velocity(Vector3::unit_x()));
        });
    }
}

pub struct BenchmarkArchetype(AllArchetypes);

impl BenchmarkArchetype {
    pub fn new() -> Self {
        BenchmarkArchetype(AllArchetypes::default())
    }

    pub fn run(&mut self) {
        (0..10000).for_each(|id| {
            let _ = self.0.insert_bundle(
                id,
                (
                    Transform(Matrix4::<f32>::from_scale(1.0)),
                    Position(Vector3::unit_x()),
                    Rotation(Vector3::unit_x()),
                    Velocity(Vector3::unit_x()),
                ),
            );
        })
    }
}
