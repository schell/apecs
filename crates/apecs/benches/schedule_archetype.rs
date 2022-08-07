use apecs::{anyhow, storage::archetype::*, system::*, world::*, CanFetch, Write};

struct A(f32);
struct B(f32);
struct C(f32);
struct D(f32);
struct E(f32);

fn ab_system(query: Query<(&mut A, &mut B)>) -> anyhow::Result<ShouldContinue> {
    query.for_each(|(a, b)| {
        std::mem::swap(&mut a.0, &mut b.0);
    });
    ok()
}

fn cd_system(query: Query<(&mut C, &mut D)>) -> anyhow::Result<ShouldContinue> {
    query.for_each(|(c, d)| {
        std::mem::swap(&mut c.0, &mut d.0);
    });
    ok()
}

fn ce_system(query: Query<(&mut C, &mut E)>) -> anyhow::Result<ShouldContinue> {
    query.for_each(|(c, e)| {
        std::mem::swap(&mut c.0, &mut e.0);
    });
    ok()
}

pub struct Benchmark(World);

impl Benchmark {
    pub fn new() -> Self {
        let mut archs = AllArchetypes::default();
        archs.insert_archetype(
            ArchetypeBuilder::default()
                .with_components(0, (0..10_000).map(|_| A(0.0)))
                .with_components(0, (0..10_000).map(|_| B(0.0)))
                .build()
        );
        archs.insert_archetype(
            ArchetypeBuilder::default()
                .with_components(10_000, (0..10_000).map(|_| A(0.0)))
                .with_components(10_000, (0..10_000).map(|_| B(0.0)))
                .with_components(10_000, (0..10_000).map(|_| C(0.0)))
                .build()
        );
        archs.insert_archetype(
            ArchetypeBuilder::default()
                .with_components(20_000, (0..10_000).map(|_| A(0.0)))
                .with_components(20_000, (0..10_000).map(|_| B(0.0)))
                .with_components(20_000, (0..10_000).map(|_| C(0.0)))
                .with_components(20_000, (0..10_000).map(|_| D(0.0)))
                .build()
        );
        archs.insert_archetype(
            ArchetypeBuilder::default()
                .with_components(30_000, (0..10_000).map(|_| A(0.0)))
                .with_components(30_000, (0..10_000).map(|_| B(0.0)))
                .with_components(30_000, (0..10_000).map(|_| C(0.0)))
                .with_components(30_000, (0..10_000).map(|_| D(0.0)))
                .with_components(30_000, (0..10_000).map(|_| E(0.0)))
                .build()
        );

        let mut world = World::default();
        world
            .with_resource(archs)
            .unwrap()
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
