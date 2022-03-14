use std::{collections::HashMap, time::Instant};

use apecs::{CanFetch, Facade, Read, Write};
use tracing::Level;

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

async fn create_system(mut facade: Facade) -> anyhow::Result<()> {
    let CreateSystemData {
        mut entities,
        mut positions,
        mut velocities,
    } = facade.fetch::<CreateSystemData>().await?;

    let a = entities.create_entity();
    let _ = positions.0.insert(a, Position { x: 0.0 });
    let _ = velocities.0.insert(a, Velocity { x: 1.0 });

    let b = entities.create_entity();
    let _ = positions.0.insert(b, Position { x: 79.0 });
    let _ = velocities.0.insert(b, Velocity { x: -1.0 });

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

fn main() -> anyhow::Result<()> {
    // a builder for `FmtSubscriber`.
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        // `ng` will TRACE, so if you don't want to see all the engine output
        // set something higher than `Level::TRACE`.
        .with_max_level(Level::INFO)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let mut world = apecs::World::default();

    world.add_resource(Entities {
        next_entity: 0,
        used: vec![],
    })?;
    world.add_default_resource::<Positions>()?;
    world.add_default_resource::<Velocities>()?;
    world.spawn_system("create", create_system);
    world.spawn_system("move", move_system);
    world.spawn_system("print", print_system);

    let start = Instant::now();
    for _ in 0..1000 {
        world.tick()?;
    }

    let elapsed = start.elapsed().as_secs_f32();
    tracing::info!("finished 1000 frames in {:.2}seconds", elapsed);
    tracing::info!("average {}fps", 1000.0 / elapsed);

    Ok(())
}
