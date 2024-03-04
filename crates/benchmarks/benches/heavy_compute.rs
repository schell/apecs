use apecs::*;
use cgmath::*;
use rayon::prelude::*;

#[derive(Copy, Clone)]
pub struct Transform(Matrix4<f32>);

#[derive(Copy, Clone)]
pub struct Position(Vector3<f32>);

#[derive(Copy, Clone)]
pub struct Rotation(Vector3<f32>);

#[derive(Copy, Clone)]
pub struct Velocity(Vector3<f32>);

pub struct Benchmark(Components);

impl Benchmark {
    pub fn new() -> Result<Self, GraphError> {
        let mut archs = Components::default();
        archs.extend::<(Transform, Position, Rotation, Velocity)>((
            Box::new(
                (0..1000)
                    .map(|id| Entry::new(id, Transform(Matrix4::<f32>::from_angle_x(Rad(1.2))))),
            ),
            Box::new((0..1000).map(|id| Entry::new(id, Position(Vector3::unit_x())))),
            Box::new((0..1000).map(|id| Entry::new(id, Rotation(Vector3::unit_x())))),
            Box::new((0..1000).map(|id| Entry::new(id, Velocity(Vector3::unit_x())))),
        ));

        Ok(Self(archs))
    }

    pub fn run(&mut self) {
        let mut q = self.0.query::<(&mut Position, &mut Transform)>();
        q.par_iter_mut().for_each(|(pos, mat)| {
            use cgmath::Transform;
            for _ in 0..100 {
                mat.0 = mat.0.invert().unwrap();
            }
            pos.0 = mat.0.transform_vector(pos.0);
        });
    }
}
