use apecs::*;

pub struct A(f32);
pub struct B(f32);

pub struct Benchmark(Components);

impl Benchmark {
    pub fn new() -> Self {
        let mut all = Components::default();
        all.extend::<(A,)>(Box::new((0..10_000).map(|id| Entry::new(id, A(0.0)))));
        Self(all)
    }

    pub fn run(&mut self) {
        for id in 0..10000 {
            let _ = self.0.insert_component(id, B(0.0));
        }

        for id in 0..10000 {
            let _ = self.0.remove_component::<B>(id);
        }
    }
}
