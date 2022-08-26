use apecs::*;

pub struct Data(f32);

macro_rules! create_entities {
    ($all:ident; $ent:ident; $( $variants:ident ),*) => {
        $(
            struct $variants(f32);
            (0..20)
                .for_each(|n| {
                    let _ = $all.insert_bundle($ent, ($variants(0.0), Data(1.0)));
                    $ent += n;
                });
        )*
    };
}

pub fn arch() -> Components {
    let mut all: Components = Components::default();
    let mut id = 0;
    create_entities!(all; id; A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);
    all
}

pub fn tick_arch(all: &mut Components) {
    let mut q = all.query::<&mut Data>();
    q.iter_mut().for_each(|data| data.0 *= 2.0);
}
