use apecs::{
    anyhow,
    join::Join,
    storage::WorldStorage,
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

pub struct Benchmark<S2, S4>
where
    S2: WorldStorage<Component = Position>,
    S4: WorldStorage<Component = Velocity>,
{
    ps: S2, vs: S4,
}

impl<S2, S4> Benchmark<S2, S4>
where
    S2: WorldStorage<Component = Position>,
    S4: WorldStorage<Component = Velocity>,
{
    pub fn new() -> anyhow::Result<Self> {
            let mut ps = S2::new_with_capacity(10001);
            let mut vs = S4::new_with_capacity(10001);

        {
            (0..10000).for_each(|id| {
                ps.insert(id, Position(Vector3::unit_x()));
                vs.insert(id, Velocity(Vector3::unit_x()));
            });
        }

        Ok(Self {
            ps, vs,
        })
    }

    pub fn run(&mut self) {
        for (_, velocity, position) in (&self.vs, &mut self.ps).join() {
            position.0 += velocity.0;
        }
    }
}
