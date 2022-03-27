use std::time::{Duration, Instant};

use apecs::{anyhow, entities::*, join::*, storage::*, world::*, CanFetch, Facade, Read, Write};
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

struct A(f32);
struct B(f32);
struct C(f32);
struct D(f32);
struct E(f32);

#[derive(CanFetch)]
struct ABSystemData {
    a_store: Write<VecStorage<A>>,
    b_store: Write<VecStorage<B>>,
}

fn ab_system(mut data: ABSystemData) -> anyhow::Result<()> {
    for (_, a, b) in (&mut data.a_store, &mut data.b_store).join() {
        std::mem::swap(&mut a.0, &mut b.0);
    }

    Ok(())
}

#[derive(CanFetch)]
struct CDSystemData {
    c_store: Write<VecStorage<C>>,
    d_store: Write<VecStorage<D>>,
}

fn cd_system(mut data: CDSystemData) -> anyhow::Result<()> {
    for (_, c, d) in (&mut data.c_store, &mut data.d_store).join() {
        std::mem::swap(&mut c.0, &mut d.0);
    }

    Ok(())
}

#[derive(CanFetch)]
struct CESystemData {
    c_store: Write<VecStorage<C>>,
    e_store: Write<VecStorage<E>>,
}

fn ce_system(mut data: CESystemData) -> anyhow::Result<()> {
    for (_, c, e) in (&mut data.c_store, &mut data.e_store).join() {
        std::mem::swap(&mut c.0, &mut e.0);
    }

    Ok(())
}

pub fn new() -> World {
        let mut entities = Entities::default();
        let mut a_store:VecStorage<A> = VecStorage::new_with_capacity(50000);
        let mut b_store:VecStorage<B> = VecStorage::new_with_capacity(40000);
        let mut c_store:VecStorage<C> = VecStorage::new_with_capacity(30000);
        let mut d_store:VecStorage<D> = VecStorage::new_with_capacity(10000);
        let mut e_store:VecStorage<E> = VecStorage::new_with_capacity(10000);

        (0..10000).for_each(|_| {
            let e = entities.create();
            a_store.insert(e.id(), A(0.0));
        });
        (0..10000).for_each(|_| {
            let e = entities.create();
            a_store.insert(e.id(), A(0.0));
            b_store.insert(e.id(), B(0.0));
        });

        (0..10000).for_each(|_| {
            let e = entities.create();
            a_store.insert(e.id(), A(0.0));
            b_store.insert(e.id(), B(0.0));
            c_store.insert(e.id(), C(0.0));
        });
        (0..10000).for_each(|_| {
            let e = entities.create();
            a_store.insert(e.id(), A(0.0));
            b_store.insert(e.id(), B(0.0));
            c_store.insert(e.id(), C(0.0));
            d_store.insert(e.id(), D(0.0));
        });
        (0..10000).for_each(|_| {
            let e = entities.create();
            a_store.insert(e.id(), A(0.0));
            b_store.insert(e.id(), B(0.0));
            c_store.insert(e.id(), C(0.0));
            e_store.insert(e.id(), E(0.0));
        });

        let mut world = World::default();
        world
            .with_resource(entities)
            .unwrap()
            .with_resource(a_store)
            .unwrap()
            .with_resource(b_store)
            .unwrap()
            .with_resource(c_store)
            .unwrap()
            .with_resource(d_store)
            .unwrap()
            .with_resource(e_store)
            .unwrap()
            .with_system("ab", ab_system)
            .with_system("cd", cd_system)
            .with_system("ce", ce_system);

        world
}

pub fn main() -> anyhow::Result<()> {
    let start = Instant::now();
    let mut world = new();
    let mut ticks = 0;

    let waker = world.get_waker();
    let mut ctx = std::task::Context::from_waker(&waker);

    while start.elapsed() < Duration::from_secs(5) {
        world.tick_with_context(Some(&mut ctx))?;
        ticks += 1;
    }

    println!("{} ticks in 5secs", ticks);

    Ok(())
}
