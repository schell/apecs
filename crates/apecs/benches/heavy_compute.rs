use std::marker::PhantomData;

use apecs::{anyhow, entities::*, join::*, storage::*, world::*, CanFetch, IsResource, Write};
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
struct HeavyComputeData<P: IsResource, T: IsResource> {
    positions: Write<P>,
    transforms: Write<T>,
}

fn system<P, T>(mut data: HeavyComputeData<P, T>) -> anyhow::Result<()>
where
    P: WorldStorage + CanReadStorage<Component = Position>,
    T: WorldStorage + CanReadStorage<Component = Transform>,
{
    use cgmath::Transform;
    (&mut data.positions, &mut data.transforms)
        .par_join()
        .for_each(|(pos, mat)| {
            for _ in 0..100 {
                mat.0 = mat.0.invert().unwrap();
            }
            pos.0 = mat.0.transform_vector(pos.0);
        });
    Ok(())
}

pub struct Benchmark<T, P, R, V>
where
    T: IsResource,
    P: IsResource,
    R: IsResource,
    V: IsResource,
{
    world: World,
    _phantom: PhantomData<(T, P, R, V)>,
}

impl<T, P, R, V> Benchmark<T, P, R, V>
where
    P: WorldStorage + CanReadStorage<Component = Position>,
    T: WorldStorage + CanReadStorage<Component = Transform>,
    R: WorldStorage + CanReadStorage<Component = Rotation>,
    V: WorldStorage + CanReadStorage<Component = Velocity>,
{
    pub fn new() -> anyhow::Result<Self> {
        let mut entities = Entities::default();
        let mut transforms: T = T::new_with_capacity(1000);
        let mut positions: P = P::new_with_capacity(1000);
        let mut rotations: R = R::new_with_capacity(1000);
        let mut velocities: V = V::new_with_capacity(1000);
        (0..1000).for_each(|_| {
            let e = entities.create();
            transforms.insert(e.id(), Transform(Matrix4::<f32>::from_angle_x(Rad(1.2))));
            positions.insert(e.id(), Position(Vector3::unit_x()));
            rotations.insert(e.id(), Rotation(Vector3::unit_x()));
            velocities.insert(e.id(), Velocity(Vector3::unit_x()));
        });
        let mut world = World::default();
        world
            .with_resource(entities)?
            .with_resource(transforms)?
            .with_resource(positions)?
            .with_resource(rotations)?
            .with_resource(velocities)?
            .with_system("heavy_compute", system::<P, T>);

        Ok(Self {
            world,
            _phantom: PhantomData,
        })
    }

    pub fn run(&mut self) {
        self.world.tick_with_context(None).unwrap();
    }
}
