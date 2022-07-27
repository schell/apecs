//! Entity component storage traits.
// mod tracking;
// pub use tracking::*;
mod vec;
pub use vec::*;
pub mod join;
pub use join::*;

use crate::{world::World, Read, Write};

pub mod bitset {
    // TODO: Remove hibitset
    pub use hibitset::*;
}

pub type ReadStore<T> = Read<VecStorage<T>>;
pub type WriteStore<T> = Write<VecStorage<T>>;

pub trait SeparateStorageExt {
    fn with_storage<T: Send + Sync + 'static>(
        &mut self,
        store: VecStorage<T>,
    ) -> anyhow::Result<&mut Self>;

    fn with_default_storage<T: Send + Sync + 'static>(&mut self) -> anyhow::Result<&mut Self>;
}

impl SeparateStorageExt for World {
    fn with_storage<T: Send + Sync + 'static>(
        &mut self,
        store: VecStorage<T>,
    ) -> anyhow::Result<&mut Self> {
        self.with_resource(store)?
            .with_plugin(crate::plugins::entity_upkeep::plugin::<VecStorage<T>>())
    }

    fn with_default_storage<T: Send + Sync + 'static>(&mut self) -> anyhow::Result<&mut Self> {
        let store = VecStorage::<T>::default();
        self.with_resource(store)?
            .with_plugin(crate::plugins::entity_upkeep::plugin::<T>())
    }
}

#[cfg(test)]
pub mod test {
    use crate::{
        self as apecs,
        storage::{separated::*, Entry, Maybe, HasId},
        system::{ok, ShouldContinue},
        world::*,
        CanFetch,
    };

    use rayon::prelude::*;
    use std::ops::DerefMut;

    #[test]
    pub fn can_track_stores() {
        let mut vs = VecStorage::default();
        vs.insert(0, 0usize);
        vs.insert(1, 1);
        vs.insert(2, 2);

        crate::system::increment_current_iteration();

        for v in vs.iter_mut() {
            if *v.value() > 1usize {
                *v.deref_mut() *= 2;
            }
        }

        crate::system::increment_current_iteration();
    }

    pub fn make_abc_vecstorage() -> VecStorage<String> {
        let mut vs = VecStorage::default();
        assert!(vs.insert(0, "abc".to_string()).is_none());
        assert!(vs.insert(1, "def".to_string()).is_none());
        assert!(vs.insert(2, "hij".to_string()).is_none());
        assert!(vs.insert(10, "666".to_string()).is_none());

        vs
    }

    pub fn make_2468_vecstorage() -> VecStorage<i32> {
        let mut vs = VecStorage::default();
        assert!(vs.insert(0, 0).is_none());
        assert!(vs.insert(2, 2).is_none());
        assert!(vs.insert(4, 4).is_none());
        assert!(vs.insert(6, 6).is_none());
        assert!(vs.insert(8, 8).is_none());
        assert!(vs.insert(10, 10).is_none());

        vs
    }

    #[test]
    fn can_tuple_join() {
        let mut entities = Entities::default();
        let mut strings: VecStorage<String> = VecStorage::default();
        let mut numbers: VecStorage<u32> = VecStorage::default();

        let a = entities.create();
        strings.insert(a.id(), "A".to_string());
        numbers.insert(a.id(), 1u32);

        let b = entities.create();
        strings.insert(b.id(), "B".to_string());
        numbers.insert(b.id(), 2u32);

        let c = entities.create();
        strings.insert(c.id(), "C".to_string());

        for (s, n) in (&mut strings, &numbers).join() {
            s.set_value(format!("{}{}{}", s.value(), s.id(), n.value()));
        }

        assert_eq!(strings.get(a.id()), Some(&"A01".to_string()));
        assert_eq!(strings.get(b.id()), Some(&"B12".to_string()));
        assert_eq!(strings.get(c.id()), Some(&"C".to_string()));
    }

    #[test]
    fn can_par_join_system() {
        #[derive(CanFetch)]
        struct Data {
            a: Write<VecStorage<f32>>,
            b: Write<VecStorage<&'static str>>,
        }

        fn system<const IS_PAR: bool>(mut data: Data) -> anyhow::Result<ShouldContinue> {
            if IS_PAR {
                (&mut data.a, &mut data.b)
                    .par_join()
                    .for_each(|(number, _string)| {
                        *number.deref_mut() *= -1.0;
                    });
            } else {
                for (number, _string) in (&mut data.a, &mut data.b).join() {
                    number.set_value(number.value() * -1.0);
                }
            }

            ok()
        }

        fn run<const IS_PAR: bool>() -> anyhow::Result<()> {
            let desc = format!("{}", if IS_PAR { "parallel" } else { "seq" });

            let mut world = World::default();
            world
                .with_resource(VecStorage::<f32>::new_with_capacity(1000))?
                .with_resource(VecStorage::<&'static str>::new_with_capacity(1000))?
                .with_system("ab", system::<IS_PAR>)?;

            // build the world
            {
                let (mut entities, mut a, mut b): (
                    Write<Entities>,
                    Write<VecStorage<f32>>,
                    Write<VecStorage<&'static str>>,
                ) = world.fetch()?;
                for n in 0..1000 {
                    let e = entities.create();
                    a.insert(e.id(), 1.0);
                    if n % 2 == 0 {
                        b.insert(e.id(), "1.0");
                    }
                }
                // affirm the state of the world
                let sum: f32 = if IS_PAR {
                    a.par_iter()
                        .into_par_iter()
                        .flatten_iter()
                        .map(|e| e.value)
                        .sum()
                } else {
                    a.iter().map(|e| e.value).sum()
                };

                assert_eq!(sum, 1000.0, "{} sum fail", desc);
            }

            // run the world
            world.tick_sync()?;

            // assert the state of the world
            let a: Read<VecStorage<f32>> = world.fetch()?;
            let sum: f32 = a.iter().map(|e| e.value).sum();
            assert_eq!(sum, 0.0, "{} join failed", desc);

            Ok(())
        }

        run::<false>().unwrap();
        run::<true>().unwrap();
    }

    #[test]
    fn can_join_not() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();
        let joined = (&vs_abc, !&vs_246)
            .join()
            .map(|(s, t)| {
                assert!(s.id() == t.id());
                (s.id(), s.value().as_str(), t.value().clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(joined, vec![(1, "def", ()),]);

        let joined = (&vs_abc, !&vs_246)
            .par_join()
            .map(|(e, n)| (e.as_str(), n))
            .collect::<Vec<_>>();
        assert_eq!(joined, vec![("def", ()),]);
    }

    #[test]
    fn can_join_maybe() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        fn unwrap<'a, 'b: 'a>(
            iter: &'a mut impl Iterator<Item = (&'b Entry<String>, Maybe<&'b Entry<i32>>)>,
        ) -> (usize, String, Option<i32>) {
            let (s, Maybe { key, inner }) = iter.next().unwrap();
            assert!(s.id() == key);
            (
                s.id(),
                s.value().clone(),
                inner.as_ref().map(|e| *e.value()),
            )
        }

        let mut joined = (&vs_abc, vs_246.maybe()).join();
        assert_eq!(unwrap(&mut joined), (0, "abc".to_string(), Some(0)));
        assert_eq!(unwrap(&mut joined), (1, "def".to_string(), None));
        assert_eq!(unwrap(&mut joined), (2, "hij".to_string(), Some(2)));
        assert_eq!(unwrap(&mut joined), (10, "666".to_string(), Some(10)));
        assert_eq!(joined.next(), None);

        let joined = (&vs_abc, vs_246.maybe_par_iter())
            .par_join()
            .map(|(es, ns)| (es.as_str(), ns.as_ref().map(|e| e.value)))
            .collect::<Vec<_>>();
        assert_eq!(
            joined,
            vec![
                ("abc", Some(0)),
                ("def", None),
                ("hij", Some(2)),
                ("666", Some(10))
            ]
        );
    }

    #[test]
    fn can_join_a_b() {
        let vs_abc = make_abc_vecstorage();
        let vs_246 = make_2468_vecstorage();

        let mut iter = (&vs_abc, &vs_246)
            .join()
            .map(|(s, i)| (s.id(), s.as_str(), *i.value()));
        assert_eq!(iter.next(), Some((0, "abc", 0)));
        assert_eq!(iter.next(), Some((2, "hij", 2)));
        assert_eq!(iter.next(), Some((10, "666", 10)));
    }

    #[test]
    fn entities_create() {
        let mut entities = Entities::default();
        let a = entities.create();
        let b = entities.create();
        let c = entities.create();

        assert_eq!(a.id(), 0);
        assert_eq!(b.id(), 1);
        assert_eq!(c.id(), 2);
    }

    #[test]
    fn entity_create_storage_insert() {
        struct A(f32);

        let mut entities = Entities::default();
        let mut a_storage: VecStorage<A> = VecStorage::default();
        let _ = (0..10000)
            .map(|_| {
                let e = entities.create();
                let _ = a_storage.insert(e.id(), A(0.0));
                e
            })
            .collect::<Vec<_>>();
    }

    #[test]
    fn choose_system_iteration_type() {
        let max = u64::MAX as f32;
        let years: f32 = max / (1000.0 * 60.0 * 60.0 * 60.0 * 24.0 * 365.0);
        assert!(1.0 <= years, "{} years", years);
    }
}
