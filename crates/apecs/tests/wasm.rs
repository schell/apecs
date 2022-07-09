use apecs::{mpsc, world::*};
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
    world.run().unwrap();
}
