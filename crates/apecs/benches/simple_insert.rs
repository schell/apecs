use apecs::storage::{archetype::{AllArchetypes, ArchetypeBuilder}, separated::*};
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

pub struct BenchmarkArchetype;

impl BenchmarkArchetype {
    pub fn new() -> Self {
        Self
    }

    pub fn run(&mut self) {
        let ts = (0..10000).map(|_| Transform(Matrix4::<f32>::from_scale(1.0)));
        let ps = (0..10000).map(|_| Position(Vector3::unit_x()));
        let rs = (0..10000).map(|_| Rotation(Vector3::unit_x()));
        let vs = (0..10000).map(|_| Velocity(Vector3::unit_x()));
        let archetype = ArchetypeBuilder::default()
            .with_components(0, ts)
            .with_components(0, ps)
            .with_components(0, rs)
            .with_components(0, vs)
            .build();
        let mut all = AllArchetypes::default();
        all.insert_archetype(archetype);
    }
}
