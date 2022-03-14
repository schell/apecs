use std::collections::HashMap;

use apecs::{CanFetch, Facade, Read, Write};
use anyhow::Context;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

pub struct Position {
    x: f32,
}

pub struct Velocity {
    x: f32,
}

pub struct Entities {
    next_entity: usize,
    used: Vec<usize>,
}

impl Entities {
    pub fn create_entity(&mut self) -> usize {
        let e = self.next_entity;
        self.next_entity += 1;
        self.used.push(e);
        e
    }

    pub fn remove_entity(&mut self, e: usize) {
        self.used.retain(|ent| *ent != e);
    }

    pub fn iter(&self) -> impl Iterator<Item = &usize> {
        self.used.iter()
    }
}

#[derive(Default)]
pub struct Positions(HashMap<usize, Position>);

#[derive(Default)]
pub struct Velocities(HashMap<usize, Velocity>);

#[derive(CanFetch)]
struct CreateSystemData<'a> {
    entities: Write<'a, Entities>,
    positions: Write<'a, Positions>,
    velocities: Write<'a, Velocities>,
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
        let a = entities.create_entity();
        let _ = positions.0.insert(a, Position { x: x as f32 });
        let _ = velocities.0.insert(a, Velocity { x: 1.0 });
        n -= 1;
    }

    Ok(())
}

#[derive(CanFetch)]
pub struct MoveSystemData<'a> {
    entities: Read<'a, Entities>,
    positions: Write<'a, Positions>,
    velocities: Write<'a, Velocities>,
}

async fn move_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let MoveSystemData {
            entities,
            mut positions,
            mut velocities,
        } = facade.fetch::<MoveSystemData>().await?;
        for entity in entities.iter() {
            match (positions.0.get_mut(entity), velocities.0.get_mut(entity)) {
                (Some(position), Some(velocity)) => {
                    position.x += velocity.x;
                    if position.x >= 79.0 {
                        velocity.x = -1.0;
                    } else if position.x <= 0.0 {
                        velocity.x = 1.0;
                    }
                }
                _ => continue,
            }
        }
    }
}

async fn print_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let positions = facade.fetch::<Read<Positions>>().await?;
        let mut line = vec![" "; 80];
        for (_, position) in positions.0.iter() {
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

                world
                    .add_resource(Entities {
                        next_entity: 0,
                        used: vec![],
                    })
                    .unwrap();
                world.add_default_resource::<Positions>().unwrap();
                world.add_default_resource::<Velocities>().unwrap();
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
