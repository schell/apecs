use apecs::*;
use cgmath::*;

pub struct Transform(Matrix4<f32>);
pub struct Position(Vector3<f32>);
pub struct Rotation(Vector3<f32>);
pub struct Velocity(Vector3<f32>);

pub struct Benchmark;

impl Benchmark {
    pub fn new() -> Self {
        Self
    }

    pub fn run(&mut self) {
        let mut all = Components::default();
        let ts = Box::new(
            (0..10000).map(|id| Entry::new(id, Transform(Matrix4::<f32>::from_scale(1.0)))),
        );
        let ps = Box::new((0..10000).map(|id| Entry::new(id, Position(Vector3::unit_x()))));
        let rs = Box::new((0..10000).map(|id| Entry::new(id, Rotation(Vector3::unit_x()))));
        let vs = Box::new((0..10000).map(|id| Entry::new(id, Velocity(Vector3::unit_x()))));
        all.extend::<(Transform, Position, Rotation, Velocity)>((ts, ps, rs, vs));
    }
}
