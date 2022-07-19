use criterion::{criterion_group, criterion_main, Criterion, Throughput};

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

    group.bench_function("apecs::separate", |b| {
        let mut bench = simple_insert::BenchmarkSeparate::new();
        b.iter(move || bench.run());
    });
    group.bench_function("apecs::archetype", |b| {
        let mut bench = simple_insert::BenchmarkArchetype::new();
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

    group.bench_function("apecs::separate", |b| {
        let mut bench = add_remove::Benchmark::new();
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
    group.bench_function("apecs::separate", |b| {
        let mut bench = simple_iter::BenchmarkSeparate::new()
        .unwrap();
        b.iter(move || bench.run());
    });
    group.bench_function("apecs::archetype", |b| {
        let mut bench = simple_iter::BenchmarkArchetype::new()
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
        let mut bench = heavy_compute::Benchmark::new()
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
    bench_range_vs_options,
    bench_add_remove,
    bench_simple_iter,
    bench_simple_insert,
    bench_frag_iter,
    bench_schedule,
    bench_heavy_compute
);
criterion_main!(benches);
