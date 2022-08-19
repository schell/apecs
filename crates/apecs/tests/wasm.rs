use apecs::{mpsc, world::*, Write, system::AsyncSystemFuture};
use wasm_bindgen_test::*;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn can_run_async() {
    let (tx, rx) = mpsc::bounded(1);
    let mut world = World::default();
    world
        .with_async(async move {
            tx.send(()).await.unwrap();
        })
        .with_async(async move {
            rx.recv().await.unwrap();
        });
    world.run();
}

#[wasm_bindgen_test]
fn parallelism() {
    let mut world = World::default();
    world
        .with_resource(0u32).unwrap()
        .with_async_system("one", |mut facade: Facade| -> AsyncSystemFuture {
            Box::pin(async move {
                let mut number: Write<u32> = facade.fetch().await?;
                *number = 1;
                Ok(())
            })
        })
        .with_async_system("two", |mut facade: Facade| -> AsyncSystemFuture {
            Box::pin(async move {
                for _ in 0..2 {
                    let mut number: Write<u32> = facade.fetch().await?;
                    *number = 2;
                }
                Ok(())
            })
        })
        .with_async_system("three", |mut facade: Facade| -> AsyncSystemFuture {
            Box::pin(async move {
                for _ in 0..3 {
                    let mut number: Write<u32> = facade.fetch().await?;
                    *number = 3;
                }
                Ok(())
            })
        })
        .with_parallelism(Parallelism::Automatic);
    world.run();
}
