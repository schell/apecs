use std::time::{Duration, Instant};

use apecs::{anyhow, join::*, storage::*, CanFetch, Facade, Read, World, Write};
use cgmath::*;

#[derive(Copy, Clone)]
struct Transform(Matrix4<f32>);

#[derive(Copy, Clone)]
struct Position(Vector3<f32>);

#[derive(Copy, Clone)]
struct Rotation(Vector3<f32>);

#[derive(Copy, Clone)]
struct Velocity(Vector3<f32>);

#[derive(CanFetch)]
struct Data {
    es: Write<Entities>,
    ts: Write<VecStorage<Transform>>,
    ps: Write<VecStorage<Position>>,
    rs: Write<VecStorage<Rotation>>,
    vs: Write<VecStorage<Velocity>>,
}

#[derive(CanFetch)]
struct SimpleIterData {
    vs: Read<VecStorage<Velocity>>,
    ps: Write<VecStorage<Position>>,
}

async fn async_run(mut facade: Facade) -> anyhow::Result<()> {
    let SimpleIterData { vs, mut ps } = facade.fetch().await.unwrap();

    for (_, velocity, position) in (&vs, &mut ps).join() {
        position.0 += velocity.0;
    }

    Ok(())
}

fn _sync_run(SimpleIterData { vs, mut ps }: SimpleIterData) -> anyhow::Result<()> {
    for (_, velocity, position) in (&vs, &mut ps).join() {
        position.0 += velocity.0;
    }

    Ok(())
}

pub fn new() -> World {
    let mut world = World::default();
    world
        .with_default_resource::<Entities>()
        .unwrap()
        .with_default_resource::<VecStorage<Transform>>()
        .unwrap()
        .with_default_resource::<VecStorage<Position>>()
        .unwrap()
        .with_default_resource::<VecStorage<Rotation>>()
        .unwrap()
        .with_default_resource::<VecStorage<Velocity>>()
        .unwrap()
        .with_async_system("simple_iter_async", async_run);
    //.with_system("simple_iter", sync_run);

    {
        let Data {
            mut es,
            mut ts,
            mut ps,
            mut rs,
            mut vs,
        } = world.fetch().unwrap();

        (0..10000).for_each(|_| {
            let e = es.create();
            let id = e.id();
            ts.insert(id, Transform(Matrix4::<f32>::from_scale(1.0)));
            ps.insert(id, Position(Vector3::unit_x()));
            rs.insert(id, Rotation(Vector3::unit_x()));
            vs.insert(id, Velocity(Vector3::unit_x()));
        });
    }

    world
}

pub fn main() -> anyhow::Result<()> {
    let start = Instant::now();
    let mut world = new();

    while start.elapsed() < Duration::from_secs(5) {
        for _ in 0..100 {
            world.tick()?;
        }
    }

    Ok(())
}
