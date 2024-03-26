//! Tests for bugs we've encountered.
use apecs::{graph, ok, Facade, Graph, GraphError, View, ViewMut, World};

struct Channel {
    tx: async_broadcast::Sender<()>,
    rx: async_broadcast::Receiver<()>,
    ok_to_drop: bool,
}

impl Default for Channel {
    fn default() -> Self {
        let (tx, rx) = async_broadcast::broadcast(3);
        Channel {
            tx,
            rx,
            ok_to_drop: false,
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
    fn ticker((chan, mut tick): (ViewMut<Channel>, ViewMut<usize>)) -> Result<(), GraphError> {
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
            .visit(|chan: View<Channel>| chan.rx.clone())
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
            .visit(|(unit, mut channel): (View<()>, ViewMut<Channel>)| {
                let () = *unit;
                channel.ok_to_drop = true;
            })
            .await
            .unwrap();

        log::info!("async exiting");
    }

    let mut world = World::default();
    let facade = world.facade();
    let task = smol::spawn(race(facade));
    world.add_subgraph(graph!(ticker));

    while !task.is_finished() {
        println!("tick");
        world.tick().unwrap();
        println!("tock");
        world.get_facade_schedule().unwrap().run().unwrap();
    }
    log::info!("executor is empty, ending the test");
}

#[test]
fn readme() {}
