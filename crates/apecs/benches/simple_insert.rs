use std::marker::PhantomData;

use apecs::{
    storage::{CanWriteStorage, WorldStorage},
    world::*,
    Write,
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

pub struct Benchmark<
    T: WorldStorage<Component = Transform>,
    P: WorldStorage<Component = Position>,
    R: WorldStorage<Component = Rotation>,
    V: WorldStorage<Component = Velocity>,
>(PhantomData<(T, P, R, V)>);

impl<T, P, R, V> Benchmark<T, P, R, V>
where
    T: WorldStorage<Component = Transform>,
    P: WorldStorage<Component = Position>,
    R: WorldStorage<Component = Rotation>,
    V: WorldStorage<Component = Velocity>,
{
    pub fn new() -> Self {
        Self(PhantomData)
    }

    pub fn run(&mut self) {
        let mut world = World::default();
        world
            .with_resource(T::new_with_capacity(10000))
            .unwrap()
            .with_resource(P::new_with_capacity(10000))
            .unwrap()
            .with_resource(R::new_with_capacity(10000))
            .unwrap()
            .with_resource(V::new_with_capacity(10000))
            .unwrap();

        let (mut es, mut ts, mut ps, mut rs, mut vs) = world
            .fetch::<(Write<Entities>, Write<T>, Write<P>, Write<R>, Write<V>)>()
            .unwrap();

        (0..10000).for_each(|_| {
            let e = es.create();
            let id = e.id();
            ts.insert(id, Transform(Matrix4::<f32>::from_scale(1.0)));
            ps.insert(id, Position(Vector3::unit_x()));
            rs.insert(id, Rotation(Vector3::unit_x()));
            vs.insert(id, Velocity(Vector3::unit_x()));
        });
    }
}
