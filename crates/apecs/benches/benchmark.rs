use apecs::{CanFetch, Facade, Read, Write, storage::*};
use anyhow::Context;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

#[derive(Clone)]
pub struct Position {
    x: f32,
}

#[derive(Clone)]
pub struct Velocity {
    x: f32,
}

#[derive(CanFetch)]
struct CreateSystemData<'a> {
    entities: Write<'a, Entities>,
    positions: Write<'a, VecStorage<Position>>,
    velocities: Write<'a, VecStorage<Velocity>>,
}

async fn create_n_system(mut facade: Facade, mut n: usize) -> anyhow::Result<()> {
    let CreateSystemData {
        mut entities,
        mut positions,
        mut velocities,
    } = facade.fetch::<CreateSystemData>().await?;

    let mut xs = (0..79usize).cycle();
    while n > 0 {
        let x = xs.next().context("could not get x")?;
        let a = entities.create();
        let _ = positions.insert(a.id(), Position { x: x as f32 });
        let _ = velocities.insert(a.id(), Velocity { x: 1.0 });
        n -= 1;
    }

    Ok(())
}

#[derive(CanFetch)]
pub struct MoveSystemData<'a> {
    positions: Write<'a, VecStorage<Position>>,
    velocities: Write<'a, VecStorage<Velocity>>,
}

async fn move_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let MoveSystemData {
            mut positions,
            mut velocities,
        } = facade.fetch::<MoveSystemData>().await?;

        for (mut position, mut velocity) in (&mut positions, &mut velocities).join() {
            position.x += velocity.x;
            if position.x >= 79.0 {
                velocity.x = -1.0;
            } else if position.x <= 0.0 {
                velocity.x = 1.0;
            }
        }
    }
}

async fn print_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let positions = facade.fetch::<Read<VecStorage<Position>>>().await?;
        let mut line = vec![" "; 80];
        for position in positions.join() {
            anyhow::ensure!(
                position.x >= 0.0 && position.x <= 79.0,
                "{} is an invalid position",
                position.x
            );
            line[position.x.floor() as usize] = "x";
        }

        tracing::debug!("{}", line.concat());
    }
}

pub fn create_move_print(c: &mut Criterion) {
    let mut group = c.benchmark_group("create_move_print");
    for size in [2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048] {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::new("1000_ticks", size), &size, |b, s| {
            b.iter(|| {
                let mut world = apecs::World::default();

                world.add_default_resource::<Entities>().unwrap();
                world.add_default_resource::<VecStorage<Position>>().unwrap();
                world.add_default_resource::<VecStorage<Velocity>>().unwrap();
                world.spawn_system("create", |f| create_n_system(f, *s));
                world.spawn_system("move", move_system);
                world.spawn_system("print", print_system);

                for _ in 0..1000 {
                    world.tick().unwrap();
                }
            })
        });
    }
    group.finish();
}

criterion_group!(benches, create_move_print);
criterion_main!(benches);
