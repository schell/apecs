use std::ops::DerefMut;

use apecs::storage::{separated::*, archetype::*};

macro_rules! create_entities_sep {
    ($store:expr; $datas:ident; $( $variants:ident ),*) => {
        $(
            struct $variants(f32);
            let mut rez = $store;
            (0..20)
                .for_each(|id| {
                    rez.insert(id, $variants(0.0));
                    $datas.insert(id, Data(1.0));
                });
        )*
    };
}

pub struct Data(f32);

pub fn tick_sep(data_storage: &mut VecStorage<Data>) {
    for data in data_storage.iter_mut() {
        data.deref_mut().0 *= 2.0;
    }
}

pub fn sep() -> VecStorage<Data> {
    let mut datas: VecStorage<Data> = VecStorage::new_with_capacity(26 * 20);
    create_entities_sep!(VecStorage::new_with_capacity(20); datas; A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);
    datas
}

macro_rules! create_entities_arch {
    ($all:ident; $( $variants:ident ),*) => {
        $(
            struct $variants(f32);
            (0..20)
                .for_each(|id| {
                    let _ = $all.insert_bundle(id, ($variants(0.0), Data(1.0)));
                });
        )*
    };
}

pub fn arch() -> AllArchetypes {
    let mut all: AllArchetypes = AllArchetypes::default();
    create_entities_arch!(all; A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);
    all
}

pub fn tick_arch(all: &mut AllArchetypes) {
    all.for_each::<&mut Data>(|mut data| data.0 *= 2.0);
}
