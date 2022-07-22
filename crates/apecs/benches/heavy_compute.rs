use apecs::{anyhow, storage::separated::*, system::*, world::*, CanFetch};
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

#[derive(CanFetch)]
struct HeavyComputeData {
    positions: WriteStore<Position>,
    transforms: WriteStore<Transform>,
}

fn system(mut data: HeavyComputeData) -> anyhow::Result<ShouldContinue> {
    use cgmath::Transform;
    (&mut data.positions, &mut data.transforms)
        .par_join()
        .for_each(|(pos, mat)| {
            for _ in 0..100 {
                mat.0 = mat.0.invert().unwrap();
            }
            pos.0 = mat.0.transform_vector(pos.0);
        });
    ok()
}

pub struct Benchmark(World);

impl Benchmark {
    pub fn new() -> anyhow::Result<Self> {
        let mut entities = Entities::default();
        let mut transforms = VecStorage::<Transform>::new_with_capacity(1000);
        let mut positions = VecStorage::<Position>::new_with_capacity(1000);
        let mut rotations = VecStorage::<Rotation>::new_with_capacity(1000);
        let mut velocities = VecStorage::<Velocity>::new_with_capacity(1000);
        (0..1000).for_each(|_| {
            let e = entities.create();
            transforms.insert(e.id(), Transform(Matrix4::<f32>::from_angle_x(Rad(1.2))));
            positions.insert(e.id(), Position(Vector3::unit_x()));
            rotations.insert(e.id(), Rotation(Vector3::unit_x()));
            velocities.insert(e.id(), Velocity(Vector3::unit_x()));
        });
        let mut world = World::default();
        world.set_resource(entities)?;
        world
            .with_resource(transforms)?
            .with_resource(positions)?
            .with_resource(rotations)?
            .with_resource(velocities)?
            .with_system("heavy_compute", system)?;

        Ok(Self(world))
    }

    pub fn run(&mut self) {
        self.0.tick_sync().unwrap();
    }
}
