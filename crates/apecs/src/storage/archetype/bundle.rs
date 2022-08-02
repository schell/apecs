//! Bundle trait for decomposing component tuples.
use std::any::TypeId;

use any_vec::AnyVec;
use anyhow::Context;
use smallvec::{smallvec, SmallVec};
use tuple_list::TupleList;

pub use tuple_list::Tuple;

pub trait TryFromAnyBundle: Sized + 'static {
    fn try_from_any(bundle: AnyBundle) -> anyhow::Result<Self>;
}

impl TryFromAnyBundle for () {
    fn try_from_any(_: AnyBundle) -> anyhow::Result<Self> {
        Ok(())
    }
}

impl<Head, Tail> TryFromAnyBundle for (Head, Tail)
where
    Head: 'static,
    Tail: TryFromAnyBundle + TupleList,
{
    fn try_from_any(mut bundle: AnyBundle) -> anyhow::Result<Self> {
        let ty = TypeId::of::<Head>();
        let index = bundle.index_of(&ty).with_context(|| {
            format!("bundle is missing type: {}", std::any::type_name::<Head>())
        })?;
        let _ = bundle.0.remove(index);
        let mut any_head_vec = bundle.1.remove(index);
        let mut head_vec = any_head_vec
            .downcast_mut::<Head>()
            .context("could not downcast")?;
        let head = head_vec
            .pop()
            .with_context(|| format!("missing '{}' component", std::any::type_name::<Head>()))?;

        Ok((head, Tail::try_from_any(bundle)?))
    }
}

pub trait TupleListTypeVec {
    fn tuple_list_type_vec() -> SmallVec<[TypeId; 4]>;
}

impl TupleListTypeVec for () {
    fn tuple_list_type_vec() -> SmallVec<[TypeId; 4]> {
        smallvec![]
    }
}

impl<Head, Tail> TupleListTypeVec for (Head, Tail)
where
    Head: 'static,
    Tail: TupleListTypeVec + TupleList,
{
    fn tuple_list_type_vec() -> SmallVec<[TypeId; 4]> {
        let mut tys = Tail::tuple_list_type_vec();
        tys.push(TypeId::of::<Head>());
        tys
    }
}

pub trait IsBundleTypeInfo: Tuple {
    fn get_type_info() -> SmallVec<[TypeId; 4]>;
}

impl<T> IsBundleTypeInfo for T
where
    T: Tuple,
    T::TupleList: TupleListTypeVec,
{
    fn get_type_info() -> SmallVec<[TypeId; 4]> {
        let types = T::TupleList::tuple_list_type_vec();
        types
    }
}

pub fn type_info<T: IsBundleTypeInfo>() -> anyhow::Result<SmallVec<[TypeId; 4]>> {
    let mut types = T::get_type_info();
    types.sort();
    ensure_type_info(&types)?;
    Ok(types)
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
    pub fn remove(&mut self, ty: &TypeId) -> Option<Self> {
        let index = self.index_of(ty)?;
        let ty = self.0.remove(index);
        let elem = self.1.remove(index);
        Some(AnyBundle(smallvec![ty], smallvec![elem]))
    }

    /// Get the type info of the bundle
    pub fn type_info(&self) -> &[TypeId] {
        &self.0
    }

    pub fn try_into_tuple<T>(self) -> anyhow::Result<T>
    where
        T: Tuple + 'static,
        T::TupleList: TryFromAnyBundle,
    {
        let tuple_list = <T::TupleList as TryFromAnyBundle>::try_from_any(self)?;
        Ok(tuple_list.into_tuple())
    }

    pub fn from_tuple<T>(tuple: T) -> Self
    where
        T: Tuple,
        AnyBundle: From<T::TupleList>,
    {
        AnyBundle::from(tuple.into_tuple_list())
    }
}

impl From<()> for AnyBundle {
    fn from(_: ()) -> Self {
        AnyBundle::default()
    }
}

impl<Head, Tail> From<(Head, Tail)> for AnyBundle
where
    Head: Send + Sync + 'static,
    Tail: TupleList + 'static,
    AnyBundle: From<Tail>,
{
    fn from((head, tail): (Head, Tail)) -> Self {
        let mut bundle = AnyBundle::from(tail);
        let mut store = AnyVec::new::<Head>();
        store.downcast_mut::<Head>().unwrap().push(head);
        bundle.insert(store.element_typeid(), store);
        bundle
    }
}

pub trait EmptyBundle {
    fn empty_bundle() -> AnyBundle;
}

impl EmptyBundle for () {
    fn empty_bundle() -> AnyBundle {
        AnyBundle::default()
    }
}

impl<Head, Tail> EmptyBundle for (Head, Tail)
where
    Head: Send + Sync + 'static,
    Tail: EmptyBundle + TupleList,
{
    fn empty_bundle() -> AnyBundle {
        let mut bundle = Tail::empty_bundle();
        let ty = TypeId::of::<Head>();
        let anyvec: AnyVec<dyn Send + Sync> = AnyVec::new::<Head>();
        bundle.insert(ty, anyvec);
        bundle
    }
}

pub fn empty_bundle<B>() -> AnyBundle
where
    B: Tuple + 'static,
    B::TupleList: EmptyBundle
{
    <B::TupleList as EmptyBundle>::empty_bundle()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn bundle_union_returns_prev() {
        let mut left = AnyBundle::from_tuple(("one".to_string(), 1.0f32, true));
        let right = AnyBundle::from_tuple(("two".to_string(), false));
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
    fn ref_bundle() {
        let a: f32 = 0.0;
        let mut b: String = "hello".to_string();
        let c: bool = true;
        let abc = (&a, &mut b, &c);
        let (_a_ref, tail) = abc.into_tuple_list();
        let (b_mut_ref, tail) = tail;
        let (_c_ref, ()) = tail;
        *b_mut_ref = "goodbye".to_string();
    }

    #[test]
    fn type_info_for_tuple() {
        let list_info =
            type_info::<<(f32, (String, (bool, (u32, ())))) as TupleList>::Tuple>().unwrap();
        let tupl_info = type_info::<(f32, String, bool, u32)>().unwrap();
        assert_eq!(list_info, tupl_info,);
        assert!(type_info::<(f32, bool, f32)>().is_err());
    }

    #[test]
    fn empty_bundle_type_info() {
        let bundle = empty_bundle::<(f32, bool, String)>();
        assert!(!bundle.0.is_empty());
        let tys = type_info::<(f32, bool, String)>().unwrap();
        assert_eq!(tys, bundle.0);
    }

    #[test]
    fn basic_type_info() {
        let tys_a = type_info::<(f32, bool, String)>().unwrap();
        assert!(!tys_a.is_empty(), "types are empty");
        let tys_b = type_info::<(bool, f32, String)>().unwrap();
        assert_eq!(tys_a, tys_b);
        assert!(tys_a == tys_b);
    }

    #[test]
    fn single_tuple_sanity() {
        let _ = type_info::<(f32,)>().unwrap();
    }
}
