use anyhow::Context;
use apecs::{
    entities::*,
    join::*,
    storage::*,
    system::*,
    world::{Facade, World},
    CanFetch, Read, Write,
};

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

fn sync_create_n_system(mut data: CreateSystemData, n: usize) -> anyhow::Result<ShouldContinue> {
    create(
        n,
        &mut data.entities,
        &mut data.positions,
        &mut data.velocities,
    )?;
    ok()
}

#[derive(CanFetch)]
pub struct MoveSystemData {
    positions: Write<VecStorage<Position>>,
    velocities: Write<VecStorage<Velocity>>,
    ticks: Write<usize>,
}

fn move_system(
    positions: &mut impl CanWriteStorage<Component = Position>,
    velocities: &mut impl CanWriteStorage<Component = Velocity>,
    ticks: &mut usize,
) {
    for (_, mut position, mut velocity) in (positions, velocities).join() {
        position.x += velocity.x;
        if position.x >= 79.0 {
            velocity.x = -1.0;
        } else if position.x <= 0.0 {
            velocity.x = 1.0;
        }
    }

    *ticks += 1;
}

async fn async_move_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let MoveSystemData {
            mut positions,
            mut velocities,
            mut ticks,
        } = facade.fetch::<MoveSystemData>().await?;
        move_system(&mut positions, &mut velocities, &mut ticks);
    }
}

fn sync_move_system(mut data: MoveSystemData) -> anyhow::Result<ShouldContinue> {
    move_system(&mut data.positions, &mut data.velocities, &mut data.ticks);
    ok()
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

fn sync_print_system(positions: Read<VecStorage<Position>>) -> anyhow::Result<ShouldContinue> {
    print_system(&positions)?;
    ok()
}

async fn async_print_system(mut facade: Facade) -> anyhow::Result<()> {
    loop {
        let positions = facade.fetch::<Read<VecStorage<Position>>>().await?;
        print_system(&positions)?;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Benchmark {
    pub size: usize,
    pub is_async: bool,
}

impl std::fmt::Display for Benchmark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format!("{}_{}", if self.is_async {"async"} else {"sync"}, self.size))
    }
}

impl Benchmark {
    pub fn run(&self) -> anyhow::Result<()> {
        let size = self.size;
        let mut world = World::default();
        world
            .with_default_resource::<Entities>()?
            .with_resource::<VecStorage<Position>>(VecStorage::new_with_capacity(size))?
            .with_resource::<VecStorage<Velocity>>(VecStorage::new_with_capacity(size))?;

        if self.is_async {
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

        for _ in 0..1000 {
            world.tick()?;
        }

        let ticks = world.fetch::<Read<usize>>().unwrap();
        assert_eq!(*ticks, 1000);

        Ok(())
    }
}
