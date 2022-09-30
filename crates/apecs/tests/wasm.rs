use apecs::{anyhow, chan::mpsc, Facade, Parallelism, World, Write};
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
    #[derive(Default)]
    struct Number(u32);

    async fn one(mut facade: Facade) -> anyhow::Result<()> {
        facade.visit(|mut number: Write<Number>| {
            number.0 = 1;
            Ok(())
        }).await
    }
    async fn two(mut facade: Facade) -> anyhow::Result<()> {
        for _ in 0..2 {
            facade.visit(|mut number: Write<Number>| {
                number.0 = 2;
                Ok(())
            }).await?;
        }
        Ok(())
    }
    async fn three(mut facade: Facade) -> anyhow::Result<()> {
        for _ in 0..3 {
            facade.visit(|mut number: Write<Number>| {
                number.0 = 3;
                Ok(())
            }).await?;
        }
        Ok(())
    }
    let mut world = World::default();
    world
        .with_resource(0u32)
        .unwrap()
        .with_async_system("one", one)
        .with_async_system("two", two)
        .with_async_system("three", three)
        .with_parallelism(Parallelism::Automatic);
    world.run();
}
