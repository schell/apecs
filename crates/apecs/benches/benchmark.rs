use std::{iter::FilterMap, slice::Iter};

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
            group.bench_with_input(BenchmarkId::new("1000_ticks", cmp), &cmp, |b, cmp| {
                b.iter(|| create_move_print(cmp.kind, cmp.size))
            });
        }
    }
    group.finish();
}

/// Measures the difference of speed of iteration between a wrapper of other std iterators
/// and a custom one.
fn bench_iter_wrapper_vs_bignext(c: &mut Criterion) {
    struct WrapperIter<'a, T>(FilterMap<Iter<'a, Option<T>>, fn(&'a Option<T>) -> Option<T>>);

    impl<'a, T: Copy> WrapperIter<'a, T> {
        fn new(s: &'a [Option<T>]) -> Self {
            WrapperIter(s.iter().filter_map(|may| may.as_ref().map(|i| *i)))
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

criterion_group!(benches, bench_create_move_print, bench_iter_wrapper_vs_bignext);
criterion_main!(benches);
