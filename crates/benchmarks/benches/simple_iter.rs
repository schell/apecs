use apecs::*;
use cgmath::*;

#[derive(Copy, Clone, Debug)]
pub struct Transform(Matrix4<f32>);

#[derive(Copy, Clone, Debug)]
pub struct Position(Vector3<f32>);

#[derive(Copy, Clone, Debug)]
pub struct Rotation(Vector3<f32>);

#[derive(Copy, Clone, Debug)]
pub struct Velocity(Vector3<f32>);

pub struct Benchmark(Components);

impl Benchmark {
    pub fn new() -> Result<Self, GraphError> {
        let ts = Box::new(
            (0..10000).map(|id| Entry::new(id, Transform(Matrix4::<f32>::from_scale(1.0)))),
        );
        let ps = Box::new((0..10000).map(|id| Entry::new(id, Position(Vector3::unit_x()))));
        let rs = Box::new((0..10000).map(|id| Entry::new(id, Rotation(Vector3::unit_x()))));
        let vs = Box::new((0..10000).map(|id| Entry::new(id, Velocity(Vector3::unit_x()))));
        let mut archs = Components::default();
        archs.extend::<(Transform, Position, Rotation, Velocity)>((ts, ps, rs, vs));
        Ok(Self(archs))
    }

    pub fn run(&mut self) {
        for (velocity, position) in self.0.query::<(&Velocity, &mut Position)>().iter_mut() {
            position.0 += velocity.0;
        }
    }
}
