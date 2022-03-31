use apecs::{entities::*, join::*, storage::*, world::*, Write, anyhow};

pub type Storage<T> = VecStorage<T>;

macro_rules! create_entities {
    ($world:ident; $datas:ident; $entities:ident; $( $variants:ident ),*) => {
        $(
            struct $variants(f32);
            let mut rez = Storage::default();
            (0..20)
                .for_each(|_| {
                    let e = $entities.create();
                    rez.insert(e.id(), $variants(0.0));
                    $datas.insert(e.id(), Data(1.0));
                });
            $world.with_resource(rez).unwrap();
        )*
    };
}

struct Data(f32);

fn run(mut data_storage: Write<Storage<Data>>) -> anyhow::Result<()> {
    for (_, data) in (&mut data_storage,).join() {
        data.0 *= 2.0;
    }

    Ok(())
}

pub struct Benchmark(World);

impl Benchmark {
    pub fn new() -> Self {
        let mut world = World::default();
        let mut entities = Entities::default();
        let mut datas = Storage::<Data>::default();
        create_entities!(world; datas; entities; A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);
        world
            .with_resource(entities)
            .unwrap()
            .with_resource(datas)
            .unwrap()
            .with_system("frag_iter", run);
        Self(world)
    }

    pub fn run(&mut self) {
        self.0.tick_with_context(None).unwrap();
    }
}
