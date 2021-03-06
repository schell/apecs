//! Implementations of `CanFetch`.
use crate as apecs;

impl apecs::CanFetch for () {
    fn reads() -> Vec<apecs::ResourceId> {
        vec![]
    }

    fn writes() -> Vec<apecs::ResourceId> {
        vec![]
    }

    fn construct(
        _: apecs::mpsc::Sender<(apecs::ResourceId, apecs::Resource)>,
        _: &mut rustc_hash::FxHashMap<apecs::ResourceId, apecs::FetchReadyResource>,
    ) -> anyhow::Result<Self> {
        Ok(())
    }
}

apecs_derive::impl_canfetch_tuple!((A, B));
apecs_derive::impl_canfetch_tuple!((A, B, C));
apecs_derive::impl_canfetch_tuple!((A, B, C, D));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E, F));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E, F, G));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E, F, G, H));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E, F, G, H, I));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E, F, G, H, I, J));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E, F, G, H, I, J, K));
apecs_derive::impl_canfetch_tuple!((A, B, C, D, E, F, G, H, I, J, K, L));
