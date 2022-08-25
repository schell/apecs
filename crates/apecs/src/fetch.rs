//! Implementations of `CanFetch`.
use crate as apecs;

impl apecs::CanFetch for () {
    fn borrows() -> Vec<apecs::internal::Borrow> {
        vec![]
    }

    fn construct(_: &mut apecs::internal::LoanManager) -> anyhow::Result<Self> {
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
