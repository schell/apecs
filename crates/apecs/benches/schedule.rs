use apecs::{
    anyhow,
    storage::*,
    system::*,
    world::*, Write,
};

struct A(f32);
struct B(f32);
struct C(f32);
struct D(f32);
struct E(f32);

fn ab_system(query: Query<(&mut A, &mut B)>) -> anyhow::Result<ShouldContinue> {
    let mut lock = query.query();
    lock.iter_mut().for_each(|(a, b)| {
        std::mem::swap(&mut a.0, &mut b.0);
    });
    ok()
}

fn cd_system(query: Query<(&mut C, &mut D)>) -> anyhow::Result<ShouldContinue> {
    let mut lock = query.query();
    lock.iter_mut().for_each(|(c, d)| {
        std::mem::swap(&mut c.0, &mut d.0);
    });
    ok()
}

fn ce_system(query: Query<(&mut C, &mut E)>) -> anyhow::Result<ShouldContinue> {
    let mut lock = query.query();
    lock.iter_mut().for_each(|(c, e)| {
        std::mem::swap(&mut c.0, &mut e.0);
    });
    ok()
}

pub struct Benchmark(World);

impl Benchmark {
    pub fn new() -> Self {
        let mut world = World::default();
        {
            let mut archs = world.fetch::<Write<ArchetypeSet>>().unwrap();
            archs.extend::<(A, B)>((
                Box::new((0..10_000).map(|id| Entry::new(id, A(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, B(0.0)))),
            ));
            archs.extend::<(A, B, C)>((
                Box::new((0..10_000).map(|id| Entry::new(id, A(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, B(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, C(0.0)))),
            ));
            archs.extend::<(A, B, C, D)>((
                Box::new((0..10_000).map(|id| Entry::new(id, A(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, B(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, C(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, D(0.0)))),
            ));
            archs.extend::<(A, B, C, D, E)>((
                Box::new((0..10_000).map(|id| Entry::new(id, A(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, B(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, C(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, D(0.0)))),
                Box::new((0..10_000).map(|id| Entry::new(id, E(0.0)))),
            ));
        }

        world
            .with_system("ab", ab_system)
            .unwrap()
            .with_system("cd", cd_system)
            .unwrap()
            .with_system("ce", ce_system)
            .unwrap()
            .with_sync_systems_run_in_parallel(true);

        Self(world)
    }

    pub fn run(&mut self) {
        self.0.tick_sync().unwrap()
    }
}
