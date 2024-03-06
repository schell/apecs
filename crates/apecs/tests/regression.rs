//! Tests for bugs we've encountered.
use std::{sync::Arc, thread::JoinHandle};

use apecs::{graph, ok, Facade, Graph, GraphError, View, ViewMut, World};

fn new_executor() -> (Arc<async_executor::Executor<'static>>, JoinHandle<()>) {
    let executor = Arc::new(async_executor::Executor::new());
    let execution_loop = {
        let executor = executor.clone();
        std::thread::spawn(move || loop {
            match Arc::strong_count(&executor) {
                1 => break,
                _ => {
                    let _ = executor.try_tick();
                }
            }
        })
    };
    (executor, execution_loop)
}

struct Channel<T> {
    tx: async_broadcast::Sender<T>,
    rx: async_broadcast::Receiver<T>,
    ok_to_drop: bool,
}

impl<T> Default for Channel<T> {
    fn default() -> Self {
        let (tx, rx) = async_broadcast::broadcast(3);
        Channel {
            tx,
            rx,
            ok_to_drop: false,
        }
    }
}

impl<T> Drop for Channel<T> {
    fn drop(&mut self) {
        if !self.ok_to_drop {
            panic!("channel should not be dropped");
        }
    }
}

#[test]
fn system_batch_drops_resources_after_racing_asyncs() {
    // ensures that resources are dropped after being sent to
    // an async that is awaiting Facade::visit, but then gets
    // cancelled

    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();

    // * each tick this increments a counter by 1
    // * when the counter reaches 3 it fires an event
    fn ticker((chan, mut tick): (ViewMut<Channel<()>>, ViewMut<usize>)) -> Result<(), GraphError> {
        *tick += 1;
        log::info!("ticked {}", *tick);
        if *tick == 3 {
            chan.tx.try_broadcast(()).unwrap();
        } else if *tick > 100 {
            panic!("really shouldn't have taken this long");
        }
        ok()
    }

    // This function infinitely loops, acquiring a unit resource and reading it
    async fn loser(facade: &mut Facade) {
        log::info!("running loser");
        loop {
            log::info!("loser awaiting View<()>");
            facade
                .visit(|unit: View<()>| {
                    log::info!("loser got View<()>");
                    let () = *unit;
                })
                .await
                .unwrap();
        }
    }

    // * races the losing async against awaiting an event from ticker
    // * after ticker wins the race, we should be able to access the unit
    //   resource, because the async executor has dropped the future which
    //   required it
    async fn race(mut facade: Facade) {
        log::info!("starting the race, awaiting View<Channel<()>>");
        let mut rx = facade
            .visit(|chan: View<Channel<()>>| chan.rx.clone())
            .await
            .unwrap();
        log::info!("race got View<Channel<()>> and is done with it");
        {
            futures_lite::future::or(
                async {
                    rx.recv().await.expect("tx was dropped");
                },
                async {
                    loser(&mut facade).await;
                    panic!("this was supposed to lose the race");
                },
            )
            .await;
        }

        log::info!("race is over");

        facade
            .visit(|(unit, mut channel): (View<()>, ViewMut<Channel<()>>)| {
                let () = *unit;
                channel.ok_to_drop = true;
            })
            .await
            .unwrap();
    }

    let (executor, _execution_loop) = new_executor();
    let mut world = World::default();
    let facade = world.facade();
    executor.spawn(race(facade)).detach();
    world.add_subgraph(graph!(ticker));

    while !executor.is_empty() {
        world.run_loop().unwrap();
        let mut facade_schedule = world.take_facade_schedule().unwrap();
        facade_schedule.run().unwrap();
    }
    log::info!("executor is empty, ending the test");
}

#[test]
fn readme() {
    use apecs::*;

    let mut world = World::default();

    // Create entities to hold heterogenous components
    let a = world.get_entities_mut().create();
    let b = world.get_entities_mut().create();

    // Nearly any type can be used as a component with zero boilerplate.
    // Here we add three components as a "bundle" to entity "a".
    world
        .get_components_mut()
        .insert_bundle(*a, (123i32, true, "abc"));
    assert!(world
        .get_components()
        .get_component::<i32>(a.id())
        .is_some());

    // Add two components as a "bundle" to entity "b".
    world.get_components_mut().insert_bundle(*b, (42i32, false));

    // Query the world for all matching bundles
    let mut query = world.get_components_mut().query::<(&mut i32, &bool)>();
    for (number, flag) in query.iter_mut() {
        println!("id: {}", number.id());
        if **flag {
            **number *= 2;
        }
    }

    // Perform random access within the same query by using the entity.
    let b_i32 = **query.find_one(b.id()).unwrap().0;
    assert_eq!(b_i32, 42);

    // Track changes to individual components
    let a_entry: &Entry<i32> = query.find_one(a.id()).unwrap().0;
    assert_eq!(apecs::current_iteration(), a_entry.last_changed());
    assert_eq!(**a_entry, 246);
}
