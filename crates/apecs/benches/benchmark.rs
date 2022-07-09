use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::{
    iter::FilterMap,
    slice::Iter,
    sync::{Arc, Mutex},
};

mod create_move_print;

mod add_remove;
mod frag_iter;
mod heavy_compute;
mod schedule;
mod simple_insert;
mod simple_iter;

mod bevy;
mod hecs;
mod legion;
mod planck_ecs;
mod shipyard;
mod specs;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Syncronicity {
    Async,
    Sync,
}

impl std::fmt::Display for Syncronicity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Syncronicity::Async => "async",
            Syncronicity::Sync => "sync",
        })
    }
}

fn bench_create_move_print(c: &mut Criterion) {
    let mut group = c.benchmark_group("create_move_print_sync");
    for size in [2, 256, 2048] {
        let cmp = create_move_print::Benchmark {
            size,
            is_async: false,
        };
        if size > 2 {
            group.sample_size(10);
        }
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input("sync", &cmp, |b, cmp| b.iter(|| cmp.run()));
    }
    group.finish();

    let mut group = c.benchmark_group("create_move_print_async");
    for size in [2, 256, 2048] {
        if size > 2 {
            group.sample_size(10);
        }
        let cmp = create_move_print::Benchmark {
            size,
            is_async: true,
        };
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input("async", &cmp, |b, cmp| b.iter(|| cmp.run()));
    }
    group.finish();
}

/// Measures the difference of speed of iteration between a wrapper of other std
/// iterators and a custom one.
fn _bench_iter_wrapper_vs_bignext(c: &mut Criterion) {
    struct WrapperIter<'a, T>(FilterMap<Iter<'a, Option<T>>, fn(&'a Option<T>) -> Option<T>>);

    impl<'a, T: Copy> WrapperIter<'a, T> {
        fn new(s: &'a [Option<T>]) -> Self {
            WrapperIter(s.iter().filter_map(|may| may.as_ref().copied()))
        }
    }

    impl<'a, T> Iterator for WrapperIter<'a, T> {
        type Item = T;

        fn next(&mut self) -> Option<Self::Item> {
            self.0.next()
        }
    }

    struct SliceIter<'a, T>(usize, &'a [Option<T>]);

    impl<'a, T> SliceIter<'a, T> {
        fn new(s: &'a [Option<T>]) -> Self {
            SliceIter(0, s)
        }

        fn get_next(&mut self) -> Option<&Option<T>> {
            if self.0 < self.1.len() {
                let item = Some(&self.1[self.0]);
                self.0 += 1;
                item
            } else {
                None
            }
        }
    }

    impl<'a, T: Copy> Iterator for SliceIter<'a, T> {
        type Item = T;

        fn next(&mut self) -> Option<Self::Item> {
            loop {
                if let Some(next) = self.get_next()? {
                    return Some(*next);
                }
            }
        }
    }

    let ns: Vec<Option<usize>> = (0usize..2usize.pow(14))
        .map(|i| if i % 3 > 0 { Some(1) } else { None })
        .collect();
    let wrapper_sum: usize = WrapperIter::new(&ns).sum();
    let bignext_sum: usize = SliceIter::new(&ns).sum();
    assert_eq!(wrapper_sum, bignext_sum);

    let mut group = c.benchmark_group("iter_wrapper_vs_bignext");

    group.throughput(Throughput::Elements(ns.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("wrapper_vs_bignext", "wrapper"),
        &(),
        |b, ()| {
            b.iter(|| {
                let sum: usize = WrapperIter::new(&ns).sum();
                sum
            })
        },
    );

    group.throughput(Throughput::Elements(ns.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("wrapper_vs_bignext", "bignext"),
        &(),
        |b, ()| {
            b.iter(|| {
                let sum: usize = SliceIter::new(&ns).sum();
                sum
            })
        },
    );

    group.finish();
}

fn _bench_mutex(c: &mut Criterion) {
    let mut mutexes = vec![];
    let mut raws = vec![];
    for n in 0..1000u32 {
        mutexes.push(Arc::new(Mutex::new(n)));
        raws.push(n);
    }

    let mut group = c.benchmark_group("component_mutation");

    group.throughput(Throughput::Elements(mutexes.len() as u64));
    group.bench_with_input(BenchmarkId::new("mutex_vs_raw", "mutex"), &(), |b, ()| {
        b.iter(|| {
            for mutex in mutexes.iter() {
                *mutex.try_lock().unwrap() = 0;
            }
        })
    });

    group.throughput(Throughput::Elements(raws.len() as u64));
    group.bench_with_input(BenchmarkId::new("mutex_vs_raw", "raw"), &(), |b, ()| {
        b.iter(|| {
            for raw in raws.iter_mut() {
                *raw = 0;
            }
        })
    });

    group.finish();
}

fn _bench_heap_vs_stack(c: &mut Criterion) {
    let mut heap = vec![0.0f32; 10_000];
    let mut stack = [0.0f32; 10_000];
    let mut heapstack = vec![[0.0f32; 10_000]];
    let mut group = c.benchmark_group("iteration_heap_vs_stack");
    group.throughput(Throughput::Elements(10_000));
    group.bench_function("heap", |b| {
        b.iter(|| {
            for n in heap.iter_mut() {
                *n = 1.0;
            }
        })
    });
    group.throughput(Throughput::Elements(10_000));
    group.bench_function("stack", |b| {
        b.iter(|| {
            for n in stack.iter_mut() {
                *n = 1.0;
            }
        })
    });
    group.throughput(Throughput::Elements(10_000));
    group.bench_function("heapstack", |b| {
        b.iter(|| {
            for n in &mut heapstack[0].iter_mut() {
                *n = 1.0;
            }
        })
    });
    group.finish();
}

fn bench_range_vs_options(c: &mut Criterion) {
    fn mk_range(ranges: &Vec<(usize, usize)>) -> Vec<(usize, usize, Vec<f32>)> {
        ranges
            .into_iter()
            .map(|(min, max)| (*min, *max, vec![0.0f32; max + 1 - min]))
            .collect()
    }

    fn mk_options(ranges: &Vec<(usize, usize)>) -> Vec<Option<f32>> {
        let mut vs = vec![];
        let mut index = 0;
        let mut ranges = ranges.into_iter();
        while let Some((rmin, rmax)) = ranges.next() {
            while index < *rmin {
                vs.push(None);
                index += 1;
            }
            while index >= *rmin && index <= *rmax {
                vs.push(Some(0.0));
                index += 1;
            }
        }
        vs
    }

    fn shift_by_one<'a>(iter: impl Iterator<Item = &'a mut f32>) {
        for n in iter {
            *n += 1.0;
        }
    }

    let ranges = vec![(500, 4999), (6000, 9_999), (15_000, 19_999)];

    let mut group = c.benchmark_group("iteration_range_vs_options");
    group.throughput(Throughput::Elements(4500 + 4000 + 5000));
    group.bench_function("options", |b| {
        let mut opts = mk_options(&ranges);
        assert_eq!(opts.len(), 20_000);

        b.iter(|| {
            let iter = opts.iter_mut().filter_map(|may| may.as_mut());
            shift_by_one(iter);
        })
    });

    group.throughput(Throughput::Elements(4500 + 4000 + 5000));
    group.bench_function("stack", |b| {
        let mut rngs = mk_range(&ranges);
        assert_eq!(rngs.len(), 3);
        assert_eq!(rngs[0].2.len(), 4500);
        assert_eq!(rngs[1].2.len(), 4000);
        assert_eq!(rngs[2].2.len(), 5000);

        b.iter(|| {
            let iter = rngs.iter_mut().flat_map(|r| r.2.iter_mut());
            shift_by_one(iter);
        })
    });
    group.finish();
}

fn bench_simple_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("simple_insert");
    // let plot_config =
    //    criterion::PlotConfiguration::default().
    // summary_scale(criterion::AxisScale::Logarithmic);
    // group.plot_config(plot_config);

    group.bench_function("apecs::storage::VecStorage", |b| {
        let mut bench = simple_insert::Benchmark::<
            apecs::storage::VecStorage<simple_insert::Transform>,
            apecs::storage::VecStorage<simple_insert::Position>,
            apecs::storage::VecStorage<simple_insert::Rotation>,
            apecs::storage::VecStorage<simple_insert::Velocity>,
        >::new();
        b.iter(move || bench.run());
    });
    group.bench_function("apecs::RangeStore", |b| {
        let mut bench = simple_insert::Benchmark::<
            apecs::storage::RangeStore<simple_insert::Transform>,
            apecs::storage::RangeStore<simple_insert::Position>,
            apecs::storage::RangeStore<simple_insert::Rotation>,
            apecs::storage::RangeStore<simple_insert::Velocity>,
        >::new();
        b.iter(move || bench.run());
    });

    group.bench_function("legion", |b| {
        let mut bench = legion::simple_insert::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("bevy", |b| {
        let mut bench = bevy::simple_insert::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("hecs", |b| {
        let mut bench = hecs::simple_insert::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("planck_ecs", |b| {
        let mut bench = planck_ecs::simple_insert::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("shipyard", |b| {
        let mut bench = shipyard::simple_insert::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("specs", |b| {
        let mut bench = specs::simple_insert::Benchmark::new();
        b.iter(move || bench.run());
    });
}

fn bench_add_remove(c: &mut Criterion) {
    let mut group = c.benchmark_group("add_remove_component");
    // let plot_config =
    //    criterion::PlotConfiguration::default().
    // summary_scale(criterion::AxisScale::Logarithmic);
    // group.plot_config(plot_config);

    group.bench_function("apecs::storage::VecStorage", |b| {
        let mut bench = add_remove::Benchmark::<
            apecs::storage::VecStorage<add_remove::A>,
            apecs::storage::VecStorage<add_remove::B>,
        >::new();
        b.iter(move || bench.run());
    });

    group.bench_function("apecs::RangeStore", |b| {
        let mut bench = add_remove::Benchmark::<
            apecs::storage::RangeStore<add_remove::A>,
            apecs::storage::RangeStore<add_remove::B>,
        >::new();
        b.iter(move || bench.run());
    });

    group.bench_function("legion", |b| {
        let mut bench = legion::add_remove::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("hecs", |b| {
        let mut bench = hecs::add_remove::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("planck_ecs", |b| {
        let mut bench = planck_ecs::add_remove::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("shipyard", |b| {
        let mut bench = shipyard::add_remove::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("specs", |b| {
        let mut bench = specs::add_remove::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("bevy", |b| {
        let mut bench = bevy::add_remove::Benchmark::new();
        b.iter(move || bench.run());
    });

    group.finish();
}

fn bench_simple_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("simple_iter");

    group.bench_function("legion", |b| {
        let mut bench = legion::simple_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("apecs::storage::VecStorage", |b| {
        let mut bench = simple_iter::Benchmark::<
            apecs::storage::VecStorage<simple_iter::Position>,
            apecs::storage::VecStorage<simple_iter::Velocity>,
        >::new()
        .unwrap();
        b.iter(move || bench.run());
    });

    group.bench_function("apecs::storage::RangeStore", |b| {
        let mut bench = simple_iter::Benchmark::<
            apecs::storage::RangeStore<simple_iter::Position>,
            apecs::storage::RangeStore<simple_iter::Velocity>,
        >::new()
        .unwrap();
        b.iter(move || bench.run());
    });

    group.bench_function("bevy", |b| {
        let mut bench = bevy::simple_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("hecs", |b| {
        let mut bench = hecs::simple_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("planck_ecs", |b| {
        let mut bench = planck_ecs::simple_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("shipyard", |b| {
        let mut bench = shipyard::simple_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("specs", |b| {
        let mut bench = specs::simple_iter::Benchmark::new();
        b.iter(move || bench.run());
    });

    group.finish();
}

fn bench_frag_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("frag_iter");

    group.bench_function("apecs::storage::VecStorage", |b| {
        let mut store = frag_iter::vec();
        b.iter(move || frag_iter::tick(&mut store))
    });
    group.bench_function("apecs::storage::RangeStore", |b| {
        let mut store = frag_iter::range();
        b.iter(move || frag_iter::tick(&mut store))
    });
    group.bench_function("legion", |b| {
        let mut bench = legion::frag_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("bevy", |b| {
        let mut bench = bevy::frag_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("hecs", |b| {
        let mut bench = hecs::frag_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("planck_ecs", |b| {
        let mut bench = planck_ecs::frag_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("shipyard", |b| {
        let mut bench = shipyard::frag_iter::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("specs", |b| {
        let mut bench = specs::frag_iter::Benchmark::new();
        b.iter(move || bench.run());
    });

    group.finish();
}

fn bench_schedule(c: &mut Criterion) {
    let mut group = c.benchmark_group("schedule");

    group.bench_function("apecs", |b| {
        let mut bench = schedule::Benchmark::new();
        b.iter(move || bench.run())
    });
    group.bench_function("legion", |b| {
        let mut bench = legion::schedule::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("bevy", |b| {
        let mut bench = bevy::schedule::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("planck_ecs", |b| {
        let mut bench = planck_ecs::schedule::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("shipyard", |b| {
        let mut bench = shipyard::schedule::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("specs", |b| {
        let mut bench = specs::schedule::Benchmark::new();
        b.iter(move || bench.run());
    });

    group.finish();
}

fn bench_heavy_compute(c: &mut Criterion) {
    let mut group = c.benchmark_group("heavy_compute");

    group.bench_function("apecs::storage::VecStorage", |b| {
        let mut bench = heavy_compute::Benchmark::<
            apecs::storage::VecStorage<heavy_compute::Transform>,
            apecs::storage::VecStorage<heavy_compute::Position>,
            apecs::storage::VecStorage<heavy_compute::Rotation>,
            apecs::storage::VecStorage<heavy_compute::Velocity>,
        >::new()
        .unwrap();
        b.iter(move || bench.run());
    });
    group.bench_function("apecs::storage::RangeStore", |b| {
        let mut bench = heavy_compute::Benchmark::<
            apecs::storage::RangeStore<heavy_compute::Transform>,
            apecs::storage::RangeStore<heavy_compute::Position>,
            apecs::storage::RangeStore<heavy_compute::Rotation>,
            apecs::storage::RangeStore<heavy_compute::Velocity>,
        >::new()
        .unwrap();
        b.iter(move || bench.run());
    });

    group.bench_function("legion", |b| {
        let mut bench = legion::heavy_compute::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("bevy", |b| {
        let mut bench = bevy::heavy_compute::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("hecs", |b| {
        let mut bench = hecs::heavy_compute::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("shipyard", |b| {
        let mut bench = shipyard::heavy_compute::Benchmark::new();
        b.iter(move || bench.run());
    });
    group.bench_function("specs", |b| {
        let mut bench = specs::heavy_compute::Benchmark::new();
        b.iter(move || bench.run());
    });
}

criterion_group!(
    benches,
    bench_create_move_print,
    bench_range_vs_options,
    bench_add_remove,
    bench_simple_iter,
    bench_simple_insert,
    bench_frag_iter,
    bench_schedule,
    bench_heavy_compute
);
criterion_main!(benches);
