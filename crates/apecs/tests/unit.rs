use apecs::*;

#[derive(Default)]
struct MyMap(std::collections::HashMap<String, u32>);

#[derive(Default)]
struct Number(u32);

#[test]
fn can_closure_system() {
    #[derive(Copy, Clone, Debug, PartialEq)]
    struct F32s(f32, f32);

    #[derive(Edges)]
    struct StatefulSystemData {
        positions: Query<&'static F32s>,
    }

    fn mk_stateful_system(
        tx: async_channel::Sender<F32s>,
    ) -> impl FnMut(StatefulSystemData) -> Result<(), GraphError> {
        println!("making stateful system");
        let mut highest_pos: F32s = F32s(0.0, f32::NEG_INFINITY);

        move |data: StatefulSystemData| {
            println!("running stateful system: highest_pos:{:?}", highest_pos);
            for pos in data.positions.query().iter_mut() {
                if pos.1 > highest_pos.1 {
                    highest_pos = *pos.value();
                    println!("set new highest_pos: {:?}", highest_pos);
                }
            }

            println!("sending highest_pos: {:?}", highest_pos);
            tx.try_send(highest_pos).map_err(GraphError::other)?;

            Ok(())
        }
    }

    let (tx, rx) = async_channel::bounded(1);

    let mut world = World::default();
    let stateful = mk_stateful_system(tx);
    world.add_subgraph(graph!(stateful));
    world
        .visit(|mut archset: ViewMut<Components>| {
            archset.insert_component(0, F32s(20.0, 30.0));
            archset.insert_component(1, F32s(0.0, 0.0));
            archset.insert_component(2, F32s(100.0, 100.0));
        })
        .unwrap();

    world.tock();

    let highest = rx.try_recv().unwrap();
    assert_eq!(F32s(100.0, 100.0), highest);
}

#[test]
fn async_run_and_return_resources() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    fn maintain_map(mut data: (Query<(&String, &u32)>, ViewMut<MyMap>)) -> Result<(), GraphError> {
        for (name, number) in data.0.query().iter_mut() {
            if !data.1 .0.contains_key(name.value()) {
                let _ = data.1 .0.insert(name.to_string(), *number.value());
            }
        }

        Ok(())
    }

    let (tx, rx) = async_channel::bounded(1);
    let mut world = World::default();
    let mut facade = world.facade();

    let task = smol::spawn(async move {
        log::info!("create running");
        tx.try_send(()).unwrap();
        facade
            .visit(
                |(mut entities, mut archset): (ViewMut<Entities>, ViewMut<Components>)| {
                    for n in 0..100u32 {
                        let e = entities.create();
                        archset.insert_bundle(e.id(), (format!("entity_{}", n), n));
                    }
                },
            )
            .await
            .unwrap();
        log::info!("async done");
    });

    world.add_subgraph(graph!(maintain_map));

    // should tick once and yield for the facade request
    world.run_loop().unwrap();
    rx.try_recv().unwrap();
    log::info!("received");

    while !task.is_finished() {
        // world sends resources to the "create" async which makes
        // entities+components
        {
            let mut facade_schedule = world.get_facade_schedule().unwrap();
            loop {
                let tick_again = facade_schedule.tick().unwrap();
                if !tick_again {
                    log::info!("schedule exhausted");
                    break;
                }
            }
        }

        world.tock();
    }

    world
        .visit(|book: View<MyMap>| {
            for n in 0..100 {
                assert_eq!(book.0.get(&format!("entity_{}", n)), Some(&n));
            }
        })
        .unwrap();
}

#[test]
fn can_create_entities_and_build_convenience() {
    struct DataA(f32);
    struct DataB(f32);

    let mut world = World::default();
    assert!(world.contains_resource::<Entities>(), "missing entities");
    let e = world
        .get_entities_mut()
        .create()
        .with_bundle((DataA(0.0), DataB(0.0)));
    let id = e.id();

    let task = smol::spawn(async move {
        println!("updating entity");
        e.with_bundle((DataA(666.0), DataB(666.0))).updates().await;
        println!("done!");
    });

    while !task.is_finished() {
        world.tock();
    }
    // just in case the executor really beat us to the punch
    world.tock();
    world
        .visit(|data: Query<(&DataA, &DataB)>| {
            let mut q = data.query();
            let (a, b) = q.find_one(id).unwrap();
            assert_eq!(666.0, a.0);
            assert_eq!(666.0, b.0);
        })
        .unwrap();
}

#[test]
fn entities_can_lazy_add_and_get() {
    #[derive(Debug, Clone, PartialEq)]
    struct Name(&'static str);

    #[derive(Debug, Clone, PartialEq)]
    struct Age(u32);

    // this tests that we can use an Entity to set components and
    // await those component updates.
    let mut world = World::default();
    let mut facade = world.facade();
    let task = smol::spawn(async move {
        let mut e = {
            let e = facade
                .visit(|mut entities: ViewMut<Entities>| entities.create())
                .await
                .unwrap();
            e
        };

        e.insert_bundle((Name("ada"), Age(666)));
        e.updates().await;

        let (name, age) = e
            .visit::<(&Name, &Age), _>(|(name, age)| (name.value().clone(), age.value().clone()))
            .await
            .unwrap();
        assert_eq!(Name("ada"), name);
        assert_eq!(Age(666), age);

        println!("done!");
    });

    while !task.is_finished() {
        world.tick().unwrap();
        world.get_facade_schedule().unwrap().run().unwrap();
    }

    world
        .visit(|ages: Query<&Age>| {
            let mut q = ages.query();
            let age = q.find_one(0).unwrap();
            assert_eq!(&Age(666), age.value());
        })
        .unwrap();
}

#[test]
fn can_query_empty_ref_archetypes_in_same_batch() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut world = World::default();
    let one = |q: Query<(&f32, &bool)>| {
        for (_f, _b) in q.query().iter_mut() {}
        ok()
    };
    let two = |q: Query<(&f32, &bool)>| {
        for (_f, _b) in q.query().iter_mut() {}
        ok()
    };
    world.add_subgraph(graph!(one, two));
    world.tock();
    let schedule = world.get_schedule_names();
    assert_eq!(vec![vec!["one", "two"]], schedule);
}

#[test]
fn deleted_entities() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    let mut world = World::default();
    {
        let entities: &mut Entities = world.get_entities_mut();
        (0..10u32).for_each(|i| {
            entities
                .create()
                .insert_bundle(("hello".to_string(), false, i))
        });
        assert_eq!(10, entities.alive_len());
    }
    world.tock();
    world
        .visit(|q: Query<&u32>| {
            assert_eq!(9, **q.query().find_one(9).unwrap());
        })
        .unwrap();
    world
        .visit(|entities: View<Entities>| {
            let entity = entities.hydrate(9).unwrap();
            entities.destroy(entity);
        })
        .unwrap();
    world.tock();
    world
        .visit(|entities: View<Entities>| {
            assert_eq!(9, entities.alive_len());
            let deleted_strings = entities.deleted_iter_of::<String>().collect::<Vec<_>>();
            assert_eq!(9, deleted_strings[0].id());
        })
        .unwrap();
}

#[test]
fn system_barrier() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    fn one(mut u32_number: ViewMut<u32>) -> Result<(), GraphError> {
        *u32_number += 1;
        end()
    }

    fn two(mut u32_number: ViewMut<u32>) -> Result<(), GraphError> {
        *u32_number += 1;
        end()
    }

    fn exit_on_three(mut f32_number: ViewMut<f32>) -> Result<(), GraphError> {
        *f32_number += 1.0;
        if *f32_number == 3.0 {
            end()
        } else {
            ok()
        }
    }

    fn lastly((u32_number, f32_number): (View<u32>, View<f32>)) -> Result<(), GraphError> {
        if *u32_number == 2 && *f32_number == 3.0 {
            end()
        } else {
            ok()
        }
    }

    let system_graph = graph!(
        // one should run before two
        one < two,
        // exit_on_three has no dependencies
        exit_on_three
    )
    .with_barrier()
    // all systems after a barrier run after the systems before a barrier
    .with_subgraph(graph!(lastly));

    let mut world = World::default();
    world.add_subgraph(system_graph);

    assert_eq!(
        vec![vec!["exit_on_three", "one",], vec!["two"], vec!["lastly"],],
        world.get_schedule_names()
    );

    world.tock();

    assert_eq!(
        vec![vec!["exit_on_three"], vec!["lastly"],],
        world.get_schedule_names()
    );

    world.tock();
    world.tock();
    assert!(world.get_schedule_names().is_empty());
}

#[test]
fn can_compile_write_without_trydefault() {
    let mut world = World::default();
    world.add_resource(0.0f32);
    {
        let _f32_num = world.get_resource_mut::<f32>().unwrap();
    }
    {
        let _f32_num = world.get_resource_mut::<f32>().unwrap();
    }
}

#[test]
fn can_tick_facades() {
    let mut world = World::default();
    world.add_resource(0u32);

    fn increment(mut counter: ViewMut<u32>) {
        *counter += 1;
    }

    for _ in 0..3 {
        let mut facade = world.facade();
        smol::spawn(async move {
            facade.visit(increment).await.unwrap();
        })
        .detach();
    }

    // give some time to accumulate all the requests
    std::thread::sleep(std::time::Duration::from_millis(10));

    world.run_loop().unwrap();
    {
        println!("first tick");
        let mut s = world.get_facade_schedule().unwrap();
        assert_eq!(3, s.len());
        s.tick().unwrap();
        while !s.unify() {}
    }

    world.run_loop().unwrap();
    {
        println!("second tick");
        let mut s = world.get_facade_schedule().unwrap();
        assert_eq!(2, s.len());
        s.tick().unwrap();
        while !s.unify() {}
    }

    world.run_loop().unwrap();
    {
        println!("third tick");
        let mut s = world.get_facade_schedule().unwrap();
        assert_eq!(1, s.len());
        s.tick().unwrap();
        while !s.unify() {}
    }

    assert_eq!(0, world.get_facade_schedule().unwrap().len());
    assert_eq!(3, world.visit(|counter: View<u32>| *counter).unwrap());
}

#[test]
fn can_derive_moongraph_edges() {
    #[derive(Edges)]
    pub struct MyData {
        pub u: View<u32>,
    }
}
