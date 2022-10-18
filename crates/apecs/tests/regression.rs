//! Tests for bugs we've encountered.
use std::sync::Arc;

use ::anyhow::Context;
use apecs::{anyhow, chan::mpmc::Channel, ok, Facade, Read, ShouldContinue, World, Write};
use futures_lite::StreamExt;

#[test]
fn can_race_asyncs() {
    // ensures that resources are dropped after being sent to
    // an async that is caling Facade::visit, but then gets
    // cancelled

    //let _ = env_logger::builder()
    //    .is_test(true)
    //    .filter_level(log::LevelFilter::Trace)
    //    .try_init();

    // each tick this increments a counter by 1
    // when the counter reaches 3 it fires an event
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

    // loops acquiring a unit resource and reading it
    async fn loser(facade: &mut Facade) -> anyhow::Result<()> {
        loop {
            println!("loser awaiting Read<()>");
            facade
                .visit(|unit: Read<()>| {
                    println!("loser got Read<()>");
                    let () = *unit;
                    Ok(())
                })
                .await?;
        }
    }

    // races the losing async against awaiting an event from ticker
    // after ticker wins the race it should be able to access the unit
    // resource
    async fn system(mut facade: Facade) -> anyhow::Result<()> {
        let mut rx = facade
            .visit(|chan: Read<Channel<()>>| Ok(chan.new_receiver()))
            .await?;

        {
            futures_lite::future::or(
                async {
                    rx.next().await.context("impossible")?;
                    anyhow::Ok(())
                },
                async {
                    loser(&mut facade).await?;
                    panic!("this was supposed to lose the race");
                },
            )
                .await?;
        }

        println!("race is over");

        facade
            .visit(|unit: Read<()>| {
                let () = *unit;
                Ok(())
            })
            .await?;

        Ok(())
    }

    let mut world = World::default();
    world
        .with_system("ticker", ticker)
        .unwrap()
        .with_async_system("system", system);

    world.tick();
    world.tick();
    world.tick();
    world.tick();
}
