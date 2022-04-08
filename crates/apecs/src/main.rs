use apecs::{entities::Entities, join::*, storage::*, CanFetch, world::Facade, Read, Write};
use tracing::Level;

#[derive(Clone)]
pub struct Position {
    x: f32,
}

#[derive(Clone)]
pub struct Velocity {
    x: f32,
}

#[derive(CanFetch)]
struct CreateSystemData {
    entities: Write<Entities>,
    positions: Write<VecStorage<Position>>,
    velocities: Write<VecStorage<Velocity>>,
}

async fn create_system(mut facade: Facade) -> anyhow::Result<()> {
    let CreateSystemData {
        mut entities,
        mut positions,
        mut velocities,
    } = facade.fetch::<CreateSystemData>().await?;

    let a = entities.create();
    let _ = positions.insert(a.id(), Position { x: 0.0 });
    let _ = velocities.insert(a.id(), Velocity { x: 1.0 });

    let b = entities.create();
    let _ = positions.insert(b.id(), Position { x: 79.0 });
    let _ = velocities.insert(b.id(), Velocity { x: -1.0 });

    Ok(())
}

#[derive(CanFetch)]
pub struct MoveSystemData {
    positions: Write<VecStorage<Position>>,
    velocities: Write<VecStorage<Velocity>>,
}

async fn move_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let MoveSystemData {
            mut positions,
            mut velocities,
        } = facade.fetch::<MoveSystemData>().await?;
        for (_, position, velocity) in (&mut positions, &mut velocities).join() {
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
        for (_, position) in (&positions,).join() {
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
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let mut world = apecs::world::World::default();
    world
        .with_default_resource::<Entities>()?
        .with_default_resource::<VecStorage<Position>>()?
        .with_default_resource::<VecStorage<Velocity>>()?
        .with_async_system("create", create_system)
        .with_async_system("move", move_system)
        .with_async_system("print", print_system);

    //let start = Instant::now();
    //for _ in 0..1000 {
    //    world.tick()?;
    //}
    world.run()?;

    //let elapsed = start.elapsed().as_secs_f32();
    //tracing::info!("finished 1000 frames in {:.2}seconds", elapsed);
    //tracing::info!("average {}fps", 1000.0 / elapsed);

    Ok(())
}
