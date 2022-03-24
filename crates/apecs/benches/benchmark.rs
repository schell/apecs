use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[path = "benchmark/create_move_print.rs"]
mod create_move_print;
use create_move_print::*;

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
    let mut group = c.benchmark_group("create_move_print");
    for kind in [Syncronicity::Async, Syncronicity::Sync] {
        for size in [2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048] {
            let cmp = CreateMovePrint { size, kind };
            group.throughput(Throughput::Elements(size as u64));
            group.bench_with_input(
                BenchmarkId::new("1000_ticks", cmp),
                &cmp,
                |b, cmp| b.iter(|| create_move_print(cmp.kind, cmp.size)),
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_create_move_print);
criterion_main!(benches);
