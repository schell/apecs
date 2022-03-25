use anyhow::Context;
use apecs::{entities::*, join::*, storage::*, CanFetch, Facade, Read, Write};

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

fn create(
    mut n: usize,
    entities: &mut Entities,
    positions: &mut impl CanWriteStorage<Component = Position>,
    velocities: &mut impl CanWriteStorage<Component = Velocity>,
) -> anyhow::Result<()> {
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

async fn async_create_n_system(mut facade: Facade, n: usize) -> anyhow::Result<()> {
    let CreateSystemData {
        mut entities,
        mut positions,
        mut velocities,
    } = facade.fetch::<CreateSystemData>().await?;
    create(n, &mut entities, &mut positions, &mut velocities)
}

fn sync_create_n_system(mut data: CreateSystemData, n: usize) -> anyhow::Result<()> {
    create(
        n,
        &mut data.entities,
        &mut data.positions,
        &mut data.velocities,
    )
}

#[derive(CanFetch)]
pub struct MoveSystemData {
    positions: Write<VecStorage<Position>>,
    velocities: Write<VecStorage<Velocity>>,
}

fn move_system(
    positions: &mut impl CanWriteStorage<Component = Position>,
    velocities: &mut impl CanWriteStorage<Component = Velocity>,
) {
    for (_, mut position, mut velocity) in (positions, velocities).join() {
        position.x += velocity.x;
        if position.x >= 79.0 {
            velocity.x = -1.0;
        } else if position.x <= 0.0 {
            velocity.x = 1.0;
        }
    }
}

async fn async_move_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let MoveSystemData {
            mut positions,
            mut velocities,
        } = facade.fetch::<MoveSystemData>().await?;
        move_system(&mut positions, &mut velocities);
    }
}

fn sync_move_system(mut data: MoveSystemData) -> anyhow::Result<()> {
    move_system(&mut data.positions, &mut data.velocities);
    Ok(())
}

fn print_system(positions: &impl CanReadStorage<Component = Position>) -> anyhow::Result<()> {
    let mut line = vec![" "; 80];
    for (_, position) in (positions,).join() {
        anyhow::ensure!(
            position.x >= 0.0 && position.x <= 79.0,
            "{} is an invalid position",
            position.x
        );
        line[position.x.floor() as usize] = "x";
    }

    tracing::debug!("{}", line.concat());
    Ok(())
}

fn sync_print_system(positions: Read<VecStorage<Position>>) -> anyhow::Result<()> {
    print_system(&positions)
}

async fn async_print_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let positions = facade.fetch::<Read<VecStorage<Position>>>().await?;
        print_system(&positions)?;
    }
}

pub fn create_move_print(syncronicity: super::Syncronicity, size: usize) -> anyhow::Result<()> {
    let mut world = apecs::world::World::default();
    world
        .with_default_resource::<Entities>()?
        .with_resource::<VecStorage<Position>>(VecStorage::new_with_capacity(size))?
        .with_resource::<VecStorage<Velocity>>(VecStorage::new_with_capacity(size))?;
    if syncronicity == super::Syncronicity::Async {
        world
            .with_async_system("create", |f| async_create_n_system(f, size))
            .with_async_system("move", async_move_system)
            .with_async_system("print", async_print_system);
    } else {
        world
            .with_system("create", move |data| sync_create_n_system(data, size))
            .with_system("move", sync_move_system)
            .with_system("print", sync_print_system);
    }
    let waker = world.get_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    for _ in 0..1000 {
        world.tick_with_context(Some(&mut cx))?;
    }

    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct CreateMovePrint {
    pub size: usize,
    pub kind: super::Syncronicity,
}

impl std::fmt::Display for CreateMovePrint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("{}_{}", self.kind, self.size))
    }
}
