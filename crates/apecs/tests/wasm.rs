use apecs::{World, Write};
use wasm_bindgen_test::*;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
fn can_run() {
    #[derive(Default)]
    pub struct Number(pub u32);

    let mut world = World::default();

    let mut facade = world.facade();
    wasm_bindgen_futures::spawn_local(async move {
        facade
            .visit(|mut number: Write<Number>| {
                number.0 = 1;
                Ok(())
            })
            .await
            .unwrap()
    });

    let mut facade = world.facade();
    wasm_bindgen_futures::spawn_local(async move {
        for _ in 0..2 {
            facade
                .visit(|mut number: Write<Number>| {
                    number.0 = 2;
                    Ok(())
                })
                .await
                .unwrap();
        }
    });

    let mut facade = world.facade();
    wasm_bindgen_futures::spawn_local(async move {
        for _ in 0..3 {
            facade
                .visit(|mut number: Write<Number>| {
                    number.0 = 3;
                    Ok(())
                })
                .await
                .unwrap();
        }
    });

    while world.facade_count() > 1 {
        world.run().unwrap();
    }

    println!("done!");
}
