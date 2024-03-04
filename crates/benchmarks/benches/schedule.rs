use apecs::*;

struct A(f32);
struct B(f32);
struct C(f32);
struct D(f32);
struct E(f32);

fn ab_system(query: Query<(&mut A, &mut B)>) -> Result<(), GraphError> {
    let mut lock = query.query();
    lock.iter_mut().for_each(|(a, b)| {
        std::mem::swap(&mut a.0, &mut b.0);
    });
    ok()
}

fn cd_system(query: Query<(&mut C, &mut D)>) -> Result<(), GraphError> {
    let mut lock = query.query();
    lock.iter_mut().for_each(|(c, d)| {
        std::mem::swap(&mut c.0, &mut d.0);
    });
    ok()
}

fn ce_system(query: Query<(&mut C, &mut E)>) -> Result<(), GraphError> {
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
        world
            .visit(
                |(mut comps, mut entities): (ViewMut<Components>, ViewMut<Entities>)| {
                    let ids = entities.create_many(10_000);
                    comps.extend::<(A, B)>((
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, A(0.0)))),
                        Box::new(ids.into_iter().map(|id| Entry::new(id, B(0.0)))),
                    ));
                    let ids = entities.create_many(10_000);
                    comps.extend::<(A, B, C)>((
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, A(0.0)))),
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, B(0.0)))),
                        Box::new(ids.into_iter().map(|id| Entry::new(id, C(0.0)))),
                    ));
                    let ids = entities.create_many(10_000);
                    comps.extend::<(A, B, C, D)>((
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, A(0.0)))),
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, B(0.0)))),
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, C(0.0)))),
                        Box::new(ids.into_iter().map(|id| Entry::new(id, D(0.0)))),
                    ));
                    let ids = entities.create_many(10_000);
                    comps.extend::<(A, B, C, D, E)>((
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, A(0.0)))),
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, B(0.0)))),
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, C(0.0)))),
                        Box::new(ids.clone().into_iter().map(|id| Entry::new(id, D(0.0)))),
                        Box::new(ids.into_iter().map(|id| Entry::new(id, E(0.0)))),
                    ));
                },
            )
            .unwrap();

        world
            .add_subgraph(graph!(ab_system, cd_system, ce_system))
            .with_parallelism(Parallelism::Automatic);

        Self(world)
    }

    pub fn run(&mut self) {
        self.0.tick_sync().unwrap()
    }
}
