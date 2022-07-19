use std::ops::DerefMut;

use apecs::{storage::separate::*, world::*};

macro_rules! create_entities {
    ($store:expr; $datas:ident; $entities:ident; $( $variants:ident ),*) => {
        $(
            struct $variants(f32);
            let mut rez = $store;
            (0..20)
                .for_each(|_| {
                    let e = $entities.create();
                    rez.insert(e.id(), $variants(0.0));
                    $datas.insert(e.id(), Data(1.0));
                });
        )*
    };
}

pub struct Data(f32);

pub fn tick(data_storage: &mut VecStorage<Data>) {
    for data in data_storage.iter_mut() {
        data.deref_mut().0 *= 2.0;
    }
}

pub fn vec() -> VecStorage<Data> {
    let mut entities = Entities::default();
    let mut datas: VecStorage<Data> = VecStorage::new_with_capacity(26 * 20);
    create_entities!(VecStorage::new_with_capacity(20); datas; entities; A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z);
    datas
}
