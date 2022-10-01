//! Bundle trait for decomposing component tuples.
use std::any::TypeId;

use any_vec::AnyVec;
use anyhow::Context;
use smallvec::{smallvec, SmallVec};

use crate as apecs;
use crate::storage::Entry;

pub(crate) trait AnyVecExt: Sized {
    fn anyvec_from_iter<C, I>(iter: I) -> Self
    where
        C: Send + Sync + 'static,
        I: IntoIterator<Item = C>,
        I::IntoIter: ExactSizeIterator;

    fn wrap(c: impl Send + Sync + Sized + 'static) -> Self {
        Self::anyvec_from_iter(Some(c))
    }
}

impl AnyVecExt for AnyVec<dyn Send + Sync + 'static> {
    fn anyvec_from_iter<C, I>(iter: I) -> Self
    where
        C: Send + Sync + 'static,
        I: IntoIterator<Item = C>,
        I::IntoIter: ExactSizeIterator,
    {
        let iter = iter.into_iter();
        let mut anyvec: Self = AnyVec::with_capacity::<C>(iter.len());
        {
            let mut vs = anyvec.downcast_mut().unwrap();
            for c in iter {
                vs.push(c);
            }
        }
        anyvec
    }
}

/// Provides runtime type info about bundles and more.
pub trait IsBundle: Sized {
    /// A bundle where each of this bundle's types are wrapped in `Entry`.
    type EntryBundle: IsBundle;
    /// A bundle where each of this bundle's types are prefixed with &'static
    /// mut.
    type MutBundle: IsBundle;

    /// Produces a list of types of a bundle, ordered by their
    /// position in the tuple.
    fn unordered_types() -> SmallVec<[TypeId; 4]>;

    /// Produces a list of types in a bundle, ordered ascending.
    ///
    /// Errors if the bundle contains duplicate types.
    fn ordered_types() -> anyhow::Result<SmallVec<[TypeId; 4]>> {
        let mut types = Self::unordered_types();
        types.sort();
        ensure_type_info(&types)?;
        Ok(types)
    }

    /// Produces an unordered list of empty `AnyVec`s to store
    /// components, with the given capacity.
    fn empty_vecs() -> SmallVec<[AnyVec<dyn Send + Sync + 'static>; 4]>;

    fn empty_any_bundle() -> anyhow::Result<AnyBundle> {
        let types = Self::ordered_types()?;
        let mut vecs = Self::empty_vecs();
        vecs.sort_by(|a, b| a.element_typeid().cmp(&b.element_typeid()));
        Ok(AnyBundle(types, vecs))
    }

    /// Produces a list of `AnyVec`s of length 1, each containing
    /// its corresponding component. The list is ordered by each component's
    /// position in the tuple.
    fn into_vecs(self) -> SmallVec<[AnyVec<dyn Send + Sync + 'static>; 4]>;

    /// Converts the tuple into a type-erased `AnyBundle`.
    ///
    /// ## Errs
    /// Errs if the tuple contains duplicate types.
    fn try_into_any_bundle(self) -> anyhow::Result<AnyBundle> {
        let types = Self::ordered_types()?;
        let mut vecs = self.into_vecs();
        vecs.sort_by(|a, b| a.element_typeid().cmp(&b.element_typeid()));
        Ok(AnyBundle(types, vecs))
    }

    fn try_from_any_bundle(bundle: AnyBundle) -> anyhow::Result<Self>;

    fn into_entry_bundle(self, entity_id: usize) -> Self::EntryBundle;

    fn from_entry_bundle(entry_bundle: Self::EntryBundle) -> Self;
}

impl<A: Send + Sync + 'static> IsBundle for (A,) {
    type EntryBundle = (Entry<A>,);
    type MutBundle = (&'static mut A,);

    fn unordered_types() -> SmallVec<[TypeId; 4]> {
        smallvec![TypeId::of::<A>()]
    }

    fn empty_vecs() -> SmallVec<[AnyVec<dyn Send + Sync + 'static>; 4]> {
        smallvec![AnyVec::new::<A>()]
    }

    fn into_vecs(self) -> SmallVec<[AnyVec<dyn Send + Sync + 'static>; 4]> {
        smallvec![AnyVec::wrap(self.0)]
    }

    fn try_from_any_bundle(mut bundle: AnyBundle) -> anyhow::Result<Self> {
        Ok((bundle.remove::<A>(&TypeId::of::<A>())?,))
    }

    fn into_entry_bundle(self, entity_id: usize) -> Self::EntryBundle {
        (Entry::new(entity_id, self.0),)
    }

    fn from_entry_bundle(entry_bundle: Self::EntryBundle) -> Self {
        (entry_bundle.0.into_inner(),)
    }
}

apecs_derive::impl_isbundle_tuple!((A, B));
apecs_derive::impl_isbundle_tuple!((A, B, C));
apecs_derive::impl_isbundle_tuple!((A, B, C, D));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E, F));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E, F, G));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E, F, G, H));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E, F, G, H, I));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E, F, G, H, I, J));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E, F, G, H, I, J, K));
apecs_derive::impl_isbundle_tuple!((A, B, C, D, E, F, G, H, I, J, K, L));

fn ensure_type_info(types: &[TypeId]) -> anyhow::Result<()> {
    for x in types.windows(2) {
        match x[0].cmp(&x[1]) {
            core::cmp::Ordering::Less => (),
            core::cmp::Ordering::Equal => {
                anyhow::bail!(
                    "attempted to allocate entity with duplicate components; each type must occur \
                     at most once!"
                )
            }
            core::cmp::Ordering::Greater => anyhow::bail!("type info is unsorted"),
        }
    }
    Ok(())
}

/// A type erased bundle of components. It is assumed that the elements are
/// ordered by typeid (ascending) at all times.
#[derive(Default)]
pub struct AnyBundle(
    pub SmallVec<[TypeId; 4]>,
    pub SmallVec<[AnyVec<dyn Send + Sync>; 4]>,
);

impl AnyBundle {
    /// Insert the component vec at the given type
    pub fn insert(
        &mut self,
        ty: TypeId,
        mut comp: AnyVec<dyn Send + Sync>,
    ) -> Option<AnyVec<dyn Send + Sync>> {
        for (i, t) in self.0.clone().into_iter().enumerate() {
            if ty < t {
                self.0.insert(i, ty);
                self.1.insert(i, comp);
                return None;
            } else if ty == t {
                std::mem::swap(&mut self.1[i], &mut comp);
                return Some(comp);
            }
        }

        self.0.push(ty);
        self.1.push(comp);
        None
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Merge the given bundle into `self`, returning any key+values in
    /// `self` that match `other`.
    pub fn union(&mut self, other: AnyBundle) -> Option<Self> {
        let mut to_caller = AnyBundle(smallvec![], smallvec![]);
        for (ty, v) in other.0.into_iter().zip(other.1.into_iter()) {
            if let Some(w) = self.insert(ty, v) {
                to_caller.insert(ty, w);
            }
        }
        if to_caller.is_empty() {
            None
        } else {
            Some(to_caller)
        }
    }

    fn index_of(&self, ty: &TypeId) -> Option<usize> {
        for (i, t) in self.0.iter().enumerate() {
            if ty == t {
                return Some(i);
            }
        }
        None
    }

    /// Remove a type from the bundle and return it as an `AnyBundle`.
    pub fn remove_any(&mut self, ty: &TypeId) -> Option<Self> {
        let index = self.index_of(ty)?;
        let ty = self.0.remove(index);
        let elem = self.1.remove(index);
        Some(AnyBundle(smallvec![ty], smallvec![elem]))
    }

    /// Remove a component from the bundle and return it.
    pub fn remove<T: 'static>(&mut self, ty: &TypeId) -> anyhow::Result<T> {
        // find the index
        let index = self
            .index_of(ty)
            .with_context(|| format!("no index of {}", std::any::type_name::<T>()))?;
        // remove the type
        let _ = self.0.remove(index);
        // remove the component type-erased vec
        let mut any_head_vec = self.1.remove(index);
        // remove the component from the vec
        let mut head_vec = any_head_vec
            .downcast_mut::<T>()
            .with_context(|| format!("could not downcast to {}", std::any::type_name::<T>()))?;
        let head = head_vec
            .pop()
            .with_context(|| format!("missing '{}' component", std::any::type_name::<T>()))?;
        Ok(head)
    }

    /// Get the type info of the bundle
    pub fn type_info(&self) -> &[TypeId] {
        &self.0
    }

    pub fn try_into_tuple<B: IsBundle>(self) -> anyhow::Result<B> {
        B::try_from_any_bundle(self)
    }

    pub fn try_from_tuple<B: IsBundle>(tuple: B) -> anyhow::Result<Self> {
        tuple.try_into_any_bundle()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn bundle_union_returns_prev() {
        let mut left = AnyBundle::try_from_tuple(("one".to_string(), 1.0f32, true)).unwrap();
        let right = AnyBundle::try_from_tuple(("two".to_string(), false)).unwrap();
        let leftover: AnyBundle = left.union(right).unwrap();
        let (a, b) = leftover.try_into_tuple::<(String, bool)>().unwrap();
        assert_eq!("one", a.as_str());
        assert_eq!(b, true);

        let (a, b, c) = left.try_into_tuple::<(String, f32, bool)>().unwrap();
        assert_eq!("two", &a);
        assert_eq!(1.0, b);
        assert_eq!(false, c);
    }

    #[test]
    fn bundle_type_info() {
        let bundle = (0.0f32,);
        let any = AnyBundle::try_from_tuple(bundle).unwrap();
        assert_eq!(&[TypeId::of::<f32>()], any.0.as_slice());
    }

    #[test]
    fn empty_bundle_type_info() {
        let bundle = <(f32, bool, String)>::empty_any_bundle().unwrap();
        assert!(!bundle.0.is_empty());
        let tys = <(f32, bool, String)>::ordered_types().unwrap();
        assert_eq!(tys, bundle.0);
    }

    #[test]
    fn basic_type_info() {
        let tys = <(f32,)>::ordered_types().unwrap();
        assert_eq!(&[TypeId::of::<f32>()], tys.as_slice());

        let tys_a = <(f32, bool, String)>::ordered_types().unwrap();
        assert!(!tys_a.is_empty(), "types are empty");

        let tys_b = <(bool, f32, String)>::ordered_types().unwrap();
        assert_eq!(tys_a, tys_b);
        assert!(tys_a == tys_b);
    }

    #[test]
    fn bundle_types_match_component_vec_types() {
        let anybundle = (0.0f32, false, "abc").try_into_any_bundle().unwrap();
        for (ty, vec) in anybundle.0.into_iter().zip(anybundle.1.into_iter()) {
            assert_eq!(ty, vec.element_typeid());
        }
    }
}
