//! Tests for bugs we've encountered.
use std::sync::Arc;

use apecs::{anyhow, chan::mpmc::Channel, ok, Facade, Read, ShouldContinue, World, Write};
use futures_lite::StreamExt;

fn new_executor() -> Arc<async_executor::Executor<'static>> {
    let executor = Arc::new(async_executor::Executor::new());
    let _execution_loop = {
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
    executor
}

#[test]
fn system_batch_drops_resources_after_racing_asyncs() {
    // ensures that resources are dropped after being sent to
    // an async that is awaiting Facade::visit, but then gets
    // cancelled

    //let _ = env_logger::builder()
    //    .is_test(true)
    //    .filter_level(log::LevelFilter::Trace)
    //    .try_init();

    // * each tick this increments a counter by 1
    // * when the counter reaches 3 it fires an event
    fn ticker(
        (mut chan, mut tick): (Write<Channel<()>>, Write<usize>),
    ) -> anyhow::Result<ShouldContinue> {
        *tick += 1;
        println!("ticked {}", *tick);
        if *tick == 3 {
            chan.try_send(()).unwrap();
        }
        ok()
    }

    // This function loops, acquiring a unit resource and reading it
    async fn loser(facade: &mut Facade) {
        loop {
            println!("loser awaiting Read<()>");
            facade
                .visit(|unit: Read<()>| {
                    println!("loser got Read<()>");
                    let () = *unit;
                    Ok(())
                })
                .await
                .unwrap();
        }
    }

    // * races the losing async against awaiting an event from ticker
    // * after ticker wins the race it should be able to access the unit
    //   resource, because the async batch runner has dropped it
    async fn race(mut facade: Facade) {
        let mut rx = facade
            .visit(|chan: Read<Channel<()>>| Ok(chan.new_receiver()))
            .await
            .unwrap();

        {
            futures_lite::future::or(
                async {
                    rx.next().await.unwrap();
                },
                async {
                    loser(&mut facade).await;
                    panic!("this was supposed to lose the race");
                },
            )
            .await;
        }

        println!("race is over");

        facade
            .visit(|unit: Read<()>| {
                let () = *unit;
                Ok(())
            })
            .await
            .unwrap();
    }

    let executor = new_executor();
    let mut world = World::default();
    let facade = world.facade();
    executor.spawn(race(facade)).detach();
    world.with_system("ticker", ticker).unwrap();

    while !executor.is_empty() {
        world.run().unwrap();
        let mut facade_schedule = world.take_facade_schedule().unwrap();
        while facade_schedule.tick().unwrap() {}
    }
}
