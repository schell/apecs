# APECS

APECS is an **A**syncronous **P**arallel **ECS**.

## Asyncronous

Most ECSs are polling (`legion` example code):

```rust,ignore
// a system fn which loops through Position and Velocity components, and reads
// the Time shared resource.
#[system(for_each)]
fn update_positions(pos: &mut Position, vel: &Velocity, #[resource] time: &Time) {
    pos.x += vel.dx * time.elapsed_seconds;
    pos.y += vel.dy * time.elapsed_seconds;
}

// construct a schedule (you should do this on init)
let mut schedule = Schedule::builder()
    .add_system(update_positions_system())
    .build();

// run our schedule (you should do this each update)
schedule.execute(&mut world, &mut resources);
```

* these systems cannot change over time (without public mutable access to `schedule`)

APECS supports permanent and temporary polling systems by returning `ShouldContinue`:
```rust,ignore
use apecs::*;

#[derive(Default)]
struct DataOne(u32);

 let mut world = world::World::default();
 world
    .with_resource(DataOne(0))
    .unwrap()
    .with_system("sys", |mut data: Write<DataOne>| -> system::ShouldContinue {
        log::info!("running sys");
        data.0 += 1;
        system::ShouldContinue::Ok
    })
    .unwrap();

world.tick().unwrap();
world.tick().unwrap();

let data: Read<DataOne> = world.fetch().unwrap();
assert_eq!(2, *data.0);
```

APECS supports permanent or temporary **async systems** as well:
```rust,ignore
use apecs::*;

#[derive(Default)]
struct DataOne(u32);

 let mut world = world::World::default();
 world
    .with_resource(DataOne(0))
    .unwrap()
    .with_async_system("sys", |facade: Facade| async move {
        log::info!("running sys");

        loop {
            let mut data: Write<DataOne> = facade.fetch().await.unwrap();
            data.0 += 1;
        }

        Ok(())
    });

world.tick().unwrap();
world.tick().unwrap();
world.tick().unwrap();

let data: Read<DataOne> = world.fetch().unwrap();
assert_eq!(2, *data.0);
```

Note the **three** world ticks here instead of the previous two.

APECS also supports running plain old futures:
```rust,ignore
use apecs::*;
let mut world = world::World::default();
world.with_async(async {
    log::info!("going to wait for a bit");
    wait_for(std::time::Duration::from_secs(20)).await;
    log::info!("that's enough! i'm done!");
});
```

## Parallel

APECS supports "outer parallelism" (that is, running systems in parallel) for syncronous systems:
```rust,ignore
// ...

let mut world = World::default();
world
    .with_resource(a_store)
    .unwrap()
    .with_resource(b_store)
    .unwrap()
    //...
    .with_system("ab", ab_system)
    .unwrap()
    .with_system("cd", cd_system)
    .unwrap()
    .with_system("ce", ce_system)
    .unwrap()
    .with_sync_systems_run_in_parallel(true);
```

It also supports "inner parallelism" (that is, joining entities and components whithin a system, in parallel):
```rust,ignore
let mut vs_abc: VecStorage<String> = make_abc_vecstorage();
let vs_246: VecStorage<u32> = make_2468_vecstorage();
(&mut vs_abc, &vs_246).par_join().for_each(|(abc, num)| {
    *abc.value() = format!("{:.2}", num);
});
```

## Examples

(go to your terminal)

## Benchmarks

[local file benchmarks](file:///Users/schell/code/apecs/target/criterion/report/index.html)
