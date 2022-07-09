//! Bundle trait for decomposing component tuples.
use std::any::TypeId;

use any_vec::AnyVec;
use anyhow::Context;
use rustc_hash::{FxHashMap, FxHashSet};

/// A type erased bundle of components.
#[derive(Default)]
pub struct AnyBundle(pub FxHashMap<TypeId, AnyVec>);

impl AnyBundle {
    /// Merge the given bundle into `self`, returning any key+values in
    /// `self` that match `other`.
    pub fn union(&mut self, other: AnyBundle) -> Option<Self> {
        let intersecting_types: FxHashSet<TypeId> = self
            .type_info()
            .intersection(&other.type_info())
            .map(Clone::clone)
            .collect();
        let types_overlap = !intersecting_types.is_empty();
        let prev = if types_overlap {
            let mut intersection = AnyBundle::default();
            for ty in intersecting_types.into_iter() {
                let value = self.0.remove(&ty).unwrap();
                intersection.0.insert(ty, value);
            }
            Some(intersection)
        } else {
            None
        };
        self.0.extend(other.0.into_iter());
        prev
    }

    /// Get the type info of the bundle
    pub fn type_info(&self) -> FxHashSet<TypeId> {
        FxHashSet::from_iter(self.0.keys().map(Clone::clone))
    }
}

pub trait IsBundle: Sized {
    type Head;
    type Tail: IsBundle;

    fn as_root() -> Option<Self> {
        None
    }

    fn uncons(self) -> Option<(Self::Head, Option<Self::Tail>)>;

    fn cons(head: Self::Head, tail: Self::Tail) -> Self;

    fn type_info() -> anyhow::Result<FxHashSet<TypeId>>
    where
        Self: 'static,
    {
        let mut types = bundle_types::<Self>();
        types.sort();
        ensure_type_info(&types)?;
        Ok(FxHashSet::from_iter(types.into_iter()))
    }

    fn into_any(self) -> AnyBundle
    where
        Self: 'static,
    {
        fn mk_any<B: IsBundle>(bundle: B, any_bundle: &mut AnyBundle)
        where
            B: 'static,
        {
            if let Some((head, may_tail)) = bundle.uncons() {
                let mut store = AnyVec::new::<B::Head>();
                store.downcast_mut::<B::Head>().unwrap().push(head);
                any_bundle.0.insert(store.element_typeid(), store);
                if let Some(tail) = may_tail {
                    mk_any::<B::Tail>(tail, any_bundle);
                }
            }
        }
        let mut bundle = AnyBundle::default();
        mk_any::<Self>(self, &mut bundle);
        bundle
    }

    fn try_from_any(mut any_bundle: AnyBundle) -> anyhow::Result<Self>
    where
        Self: 'static,
    {
        if let Some(root) = Self::as_root() {
            Ok(root)
        } else {
            let ty = TypeId::of::<Self::Head>();
            let mut any_head_vec = any_bundle.0.remove(&ty).with_context(|| {
                format!(
                    "anybundle is missing type: {}",
                    std::any::type_name::<Self::Head>()
                )
            })?;
            let mut head_vec = any_head_vec
                .downcast_mut::<Self::Head>()
                .context("could not downcast")?;
            let head = head_vec.pop().context("missing component")?;
            let tail = <Self::Tail>::try_from_any(any_bundle)?;
            Ok(Self::cons(head, tail))
        }
    }
}

impl IsBundle for () {
    type Head = ();
    type Tail = ();

    fn as_root() -> Option<Self> {
        Some(())
    }

    fn uncons(self) -> Option<(Self::Head, Option<Self::Tail>)> {
        None
    }

    fn cons((): Self::Head, (): Self::Tail) -> Self {
        ()
    }
}

impl<A> IsBundle for (A,) {
    type Head = A;
    type Tail = ();

    fn uncons(self) -> Option<(Self::Head, Option<Self::Tail>)> {
        Some((self.0, None))
    }

    fn cons(head: Self::Head, _: Self::Tail) -> Self {
        (head,)
    }
}

impl<A, B> IsBundle for (A, B) {
    type Head = A;
    type Tail = (B,);

    fn uncons(self) -> Option<(Self::Head, Option<Self::Tail>)> {
        Some((self.0, Some((self.1,))))
    }

    fn cons(head: Self::Head, (tail,): Self::Tail) -> Self {
        (head, tail)
    }
}

impl<A, B, C> IsBundle for (A, B, C) {
    type Head = A;
    type Tail = (B, C);

    fn uncons(self) -> Option<(Self::Head, Option<Self::Tail>)> {
        Some((self.0, Some((self.1, self.2))))
    }

    fn cons(head: Self::Head, (b, c): Self::Tail) -> Self {
        (head, b, c)
    }
}

pub trait Apply<F> {
    type Output;
}

impl<F> Apply<F> for () {
    type Output = ();
}

impl<F, A: Apply<F>> Apply<F> for (A,) {
    type Output = (<A as Apply<F>>::Output,);
}

impl<F, A: Apply<F>, B: Apply<F>> Apply<F> for (A, B) {
    type Output = (<A as Apply<F>>::Output, <B as Apply<F>>::Output);
}

impl<F, A: Apply<F>, B: Apply<F>, C: Apply<F>> Apply<F> for (A, B, C) {
    type Output = (
        <A as Apply<F>>::Output,
        <B as Apply<F>>::Output,
        <C as Apply<F>>::Output,
    );
}

pub fn ensure_type_info(types: &[TypeId]) -> anyhow::Result<()> {
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

fn bundle_types<B: IsBundle + 'static>() -> Vec<TypeId> {
    let mut types = vec![TypeId::of::<B::Head>()];
    if std::any::TypeId::of::<B::Tail>() != std::any::TypeId::of::<()>() {
        types.extend(bundle_types::<B::Tail>());
    }
    types
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn bundle_union_returns_prev() {
        let mut left = ("one".to_string(), 1.0f32, true).into_any();
        let right = ("two".to_string(), false).into_any();
        let leftover: AnyBundle = left.union(right).unwrap();
        let (a, b) = <(String, bool)>::try_from_any(leftover).unwrap();
        assert_eq!("one", a.as_str());
        assert_eq!(b, true);

        let (a, b, c) = <(String, f32, bool)>::try_from_any(left).unwrap();
        assert_eq!("two", &a);
        assert_eq!(1.0, b);
        assert_eq!(false, c);
    }

    #[test]
    fn ref_bundle() {
        let a: f32 = 0.0;
        let mut b: String = "hello".to_string();
        let c: bool = true;
        let abc = (&a, &mut b, &c);
        let (_a_ref, may_tail) = abc.uncons().unwrap();
        let (b_mut_ref, may_tail) = may_tail.unwrap().uncons().unwrap();
        let (_c_ref, _) = may_tail.unwrap().uncons().unwrap();
        *b_mut_ref = "goodbye".to_string();
    }
}
