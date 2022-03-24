use std::iter::Map;

use hibitset::{BitIter, BitSet};

use crate::{entities::*, storage::*};

fn sync<A, B>(
    a: &mut A,
    next_a: &mut (usize, <A::Item as StorageComponent>::Component),
    b: &mut B,
    next_b: &mut (usize, <B::Item as StorageComponent>::Component),
) -> Option<()>
where
    A: Iterator,
    <A as Iterator>::Item: StorageComponent,
    B: Iterator,
    <B as Iterator>::Item: StorageComponent,
{
    loop {
        if next_a.0 == next_b.0 {
            break;
        }
        while next_a.0 < next_b.0 {
            *next_a = a.next()?.split();
        }
        while next_b.0 < next_a.0 {
            *next_b = b.next()?.split();
        }
    }

    Some(())
}

pub struct JoinedIter<T>(T);

impl<A> Iterator for JoinedIter<(A,)>
where
    A: Iterator,
    <A as Iterator>::Item: StorageComponent,
{
    type Item = (
        usize,
        <<A as Iterator>::Item as StorageComponent>::Component,
    );

    fn next(&mut self) -> Option<Self::Item> {
        let next_a = self.0 .0.next()?.split();
        Some((next_a.0, next_a.1))
    }
}

pub trait Join {
    type Iter: Iterator;

    fn join(self) -> Self::Iter;
}

impl<'a, T: CanWriteStorage> Join for &'a mut T {
    type Iter = T::IterMut<'a>;

    fn join(self) -> Self::Iter {
        self.iter_mut()
    }
}

impl<'a, T: CanReadStorage> Join for &'a T {
    type Iter = T::Iter<'a>;

    fn join(self) -> Self::Iter {
        self.iter()
    }
}

impl<'a> Join for &'a Entities {
    type Iter = Map<BitIter<&'a BitSet>, fn(u32) -> Entry<Entity>>;

    fn join(self) -> Self::Iter {
        self.iter()
    }
}

pub struct Without<T>(pub(crate) T);

pub struct WithoutIter<T: Iterator> {
    iter: T,
    id: usize,
    next_id: usize,
}

impl<T> WithoutIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
{
    pub(crate) fn new(mut iter: T) -> Self {
        let next_id = iter.next().map(|e| e.id()).unwrap_or_else(|| usize::MAX);
        WithoutIter {
            iter,
            id: 0,
            next_id,
        }
    }
}

impl<T: Iterator> Iterator for WithoutIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
{
    type Item = Entry<()>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.id == self.next_id {
            self.next_id = self
                .iter
                .next()
                .map(|e| e.id())
                .unwrap_or_else(|| usize::MAX);
            self.id += 1;
        }

        let entry = Entry {
            key: self.id,
            value: (),
        };
        self.id += 1;
        Some(entry)
    }
}

impl<T> Join for WithoutIter<T>
where
    T: Iterator,
    T::Item: StorageComponent,
{
    type Iter = WithoutIter<T>;

    fn join(self) -> Self::Iter {
        self
    }
}

//pub struct Maybe<T>(pub T);
//
//pub trait MaybeJoin: Sized {
//    fn maybe(&self) -> Maybe<&Self>;
//    fn maybe_mut(&mut self) -> Maybe<&mut Self>;
//}
//
//impl<T: Storage> MaybeJoin for T {
//    fn maybe(&self) -> Maybe<&Self> {
//        Maybe(self)
//    }
//
//    fn maybe_mut(&mut self) -> Maybe<&mut Self> {
//        Maybe(self)
//    }
//}
//
//impl<'a, T: Storage> Join for Maybe<&'a T> {
//    type Iter = T::Iter<'a>;
//
//    fn join(self) -> JoinIter<Self::Iter> {
//        todo!()
//    }
//}
//
//impl<'a, T: Storage> Join for Maybe<&'a mut T> {}
//

impl<A: Join> Join for (A,)
where
    A::Iter: Iterator,
    <A::Iter as Iterator>::Item: StorageComponent,
{
    type Iter = JoinedIter<(A::Iter,)>;

    fn join(self) -> Self::Iter {
        JoinedIter((self.0.join(),))
    }
}

apecs_derive::impl_join_tuple!((A, B));
apecs_derive::impl_join_tuple!((A, B, C));
apecs_derive::impl_join_tuple!((A, B, C, D));
apecs_derive::impl_join_tuple!((A, B, C, D, E));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K, L));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K, L, M));
apecs_derive::impl_join_tuple!((A, B, C, D, E, F, G, H, I, J, K, L, M, N));

#[cfg(test)]
mod test {
    use crate::join::*;

    #[test]
    fn can_join_entities() {
        let entities = Entities::default();
        let abc_store: VecStorage<String> = VecStorage::default();
        for (_, _, _) in (&entities, &abc_store).join() {}
    }

    #[test]
    fn can_join_without() {
        let vs_abc = crate::storage::test::make_abc_vecstorage();
        let vs_246 = crate::storage::test::make_2468_vecstorage();

        let mut joined = (&vs_abc, !&vs_246).join();
        assert_eq!(joined.next(), Some((1, &"def".to_string(), ())));
        assert_eq!(joined.next(), None);

        let mut vs_a: VecStorage<&str> = VecStorage::default();
        vs_a.insert(0, "a0");
        vs_a.insert(1, "a1");
        vs_a.insert(3, "a3");
        vs_a.insert(4, "a4");
        vs_a.insert(8, "a8");

        let withouts = (!&vs_a,).join().take(5).map(|e| e.0).collect::<Vec<_>>();
        assert_eq!(&withouts, &[2, 5, 6, 7, 9]);
    }

    #[test]
    fn joiny_playground() {
        let mut abc_store: VecStorage<String> = VecStorage::default();
        abc_store.insert(0, "a".to_string());
        abc_store.insert(1, "b".to_string());
        abc_store.insert(2, "c".to_string());
        abc_store.insert(10, "ten".to_string());
        let mut def_store: VecStorage<String> = VecStorage::default();
        def_store.insert(1, "d".to_string());
        def_store.insert(2, "e".to_string());
        def_store.insert(3, "f".to_string());
        def_store.insert(10, "10".to_string());
        let mut hij_store: VecStorage<String> = VecStorage::default();
        hij_store.insert(2, "h".to_string());
        hij_store.insert(3, "i".to_string());
        hij_store.insert(4, "j".to_string());
        hij_store.insert(10, "9+1".to_string());

        (|| -> Option<()> {
            let mut iter_a = abc_store.iter();
            let mut iter_b = def_store.iter();
            let mut iter_c = hij_store.iter();

            let mut next_a = iter_a.next()?.split();
            let mut next_b = iter_b.next()?.split();
            let mut next_c = iter_c.next()?.split();
            while !(next_a.0 == next_b.0 && next_a.0 == next_c.0) {
                sync(&mut iter_a, &mut next_a, &mut iter_b, &mut next_b)?;
                sync(&mut iter_a, &mut next_a, &mut iter_c, &mut next_c)?;
            }

            assert_eq!(next_a.0, 2);

            Some(())
        })()
        .unwrap();

        let mut abc_def_hijs = JoinedIter((abc_store.iter(), def_store.iter(), hij_store.iter()));
        assert_eq!(
            abc_def_hijs.next().unwrap(),
            (2, &"c".to_string(), &"e".to_string(), &"h".to_string())
        );

        let abc_defs = (&abc_store, &def_store).join().collect::<Vec<_>>();
        assert_eq!(
            abc_defs,
            vec![
                (1, &"b".to_string(), &"d".to_string()),
                (2, &"c".to_string(), &"e".to_string()),
                (10, &"ten".to_string(), &"10".to_string()),
            ]
        );

        let abc_def_hijs = (&abc_store, &def_store, &hij_store)
            .join()
            .collect::<Vec<_>>();
        assert_eq!(
            abc_def_hijs,
            vec![
                (2, &"c".to_string(), &"e".to_string(), &"h".to_string()),
                (
                    10,
                    &"ten".to_string(),
                    &"10".to_string(),
                    &"9+1".to_string()
                ),
            ]
        );
    }
}
