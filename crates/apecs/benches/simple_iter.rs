use apecs::{
    anyhow,
    storage::{archetype::*, separated::*},
};
use cgmath::*;

#[derive(Copy, Clone, Debug)]
pub struct Transform(Matrix4<f32>);

#[derive(Copy, Clone, Debug)]
pub struct Position(Vector3<f32>);

#[derive(Copy, Clone, Debug)]
pub struct Rotation(Vector3<f32>);

#[derive(Copy, Clone, Debug)]
pub struct Velocity(Vector3<f32>);

pub struct BenchmarkSeparate {
    ps: VecStorage<Position>,
    vs: VecStorage<Velocity>,
}

impl BenchmarkSeparate {
    pub fn new() -> anyhow::Result<Self> {
        let mut ps = VecStorage::<Position>::new_with_capacity(10001);
        let mut vs = VecStorage::<Velocity>::new_with_capacity(10001);
        let mut ts = VecStorage::<Transform>::new_with_capacity(10001);
        let mut rs = VecStorage::<Rotation>::new_with_capacity(10001);

        (0..10000).for_each(|id| {
            ps.insert(id, Position(Vector3::unit_x()));
            vs.insert(id, Velocity(Vector3::unit_x()));
            ts.insert(id, Transform(Matrix4::from_scale(1.0)));
            rs.insert(id, Rotation(Vector3::unit_x()));
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
        let arch = ArchetypeBuilder::default()
            .with_components(0, (0..10000).map(|_| Position(Vector3::unit_x())))
            .with_components(0, (0..10000).map(|_| Velocity(Vector3::unit_x())))
            .with_components(0, (0..10000).map(|_| Transform(Matrix4::from_scale(1.0))))
            .with_components(0, (0..10000).map(|_| Rotation(Vector3::unit_x())))
            .build();
        archs.insert_archetype(arch);

        Ok(Self(archs))
    }

    pub fn run(&mut self) {
        self.0.for_each::<(&Velocity, &mut Position)>(|(velocity, position)| {
            position.0 += velocity.0;
        });
    }
}
