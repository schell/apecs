//! Build Maxwell and its dependencies.
use anyhow::Context;
use chrono::{offset::Utc, DateTime};
use clap::Parser;
use nom::{bytes::complete as bytes, character::complete as character, combinator, IResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[clap(trailing_var_arg = true)]
/// Benchmark Maxwell
pub struct Bench {
    /// Path to the long-lived benchmark yaml file
    #[clap(long)]
    benchmark_file: Option<PathBuf>,

    /// Whether to append this run to the historical benchmark file
    #[clap(long)]
    append: bool
}

impl Bench {
    fn get_store(path: impl AsRef<Path>) -> anyhow::Result<Option<HistoricalBenchmarks>> {
        if path.as_ref().is_file() {
            let contents = std::fs::read_to_string(path.as_ref()).context(format!(
                "could not read benchmark file: {}",
                path.as_ref().display()
            ))?;
            let benchmark_store: HistoricalBenchmarks =
                serde_yaml::from_str(&contents).context("could not parse benchmark file")?;
            Ok(Some(benchmark_store))
        } else {
            Ok(None)
        }
    }

    pub fn run(&self) -> anyhow::Result<()> {
        tracing::info!("running benchmarks",);

        let mut store = if let Some(file) = self.benchmark_file.as_ref() {
            Self::get_store(file)?.unwrap_or_default()
        } else {
            HistoricalBenchmarks::default()
        };
        store.sort();

        let date = Utc::now();
        if let Some(prev) = store.find_last() {
            let elapsed = date - prev.date;
            let days = elapsed.num_minutes() as f32 / (24.0 * 60.0);
            tracing::info!("{:.2}days since last stored event", days);
        }

        let output = duct::cmd!(
            "cargo",
            "bench",
            "--bench",
            "benchmark",
            "--",
            "--output-format",
            "bencher"
        )
        .stdout_capture()
        .run()
        .context("could not run benchmarks")?;
        let parser_input = String::from_utf8(output.stdout).context("output is not utf8")?;

        let (_, results) = parse_benchmarks(&parser_input).unwrap();
        tracing::debug!("parsed {} results", results.len());

        for benchmark in results.into_iter() {
            let historical_benchmark = HistoricalBenchmark {
                date: date.clone(),
                benchmark,
                tags: vec![],
            };
            if let Some(prev) = store.find_latest_by_name(&historical_benchmark.benchmark.name) {
                let change_ns = historical_benchmark.benchmark.ns - prev.benchmark.ns;
                let change_percent = (change_ns as f32 / prev.benchmark.ns as f32) * 100.0;
                if change_percent >= 10.0 {
                    tracing::error!(
                        "{} {} +{:.2}%",
                        historical_benchmark.benchmark.name,
                        historical_benchmark.benchmark.ns,
                        change_percent
                    );
                } else if change_percent > 0.0 {
                    tracing::warn!(
                        "{} {} +{:.2}%",
                        historical_benchmark.benchmark.name,
                        historical_benchmark.benchmark.ns,
                        change_percent
                    );
                } else {
                    tracing::info!(
                        "{} {} {:.2}%",
                        historical_benchmark.benchmark.name,
                        historical_benchmark.benchmark.ns,
                        change_percent
                    );
                }
            }
            store.benchmarks.push(historical_benchmark);
        }

        if self.append {
            if let Some(file) = self.benchmark_file.as_ref() {
                let contents = serde_yaml::to_string(&store).context("could not serialize store")?;
                std::fs::write(file, contents).context("could not write benchmark file")?;
                tracing::info!("saved benchmark file '{}'", file.display());
            }
        }

        Ok(())
    }
}

/// Parse the beginning of a benchmark result and return the benchmark
/// name.
fn parse_bench_result_start(i: &str) -> IResult<&str, &str> {
    let (i, _) = bytes::tag("test ")(i)?;
    let (i, name) = bytes::take_while(|c| c != ' ')(i)?;
    let (i, _) = character::char(' ')(i)?;
    let (i, _) = bytes::tag("... ")(i)?;
    Ok((i, name))
}

/// Parse the rest of a benchmark result and return the benchmark
/// nanos per iter and range.
fn parse_bench_result_rest(i: &str) -> IResult<&str, (i64, i64)> {
    let (i, _) = bytes::tag("bench:")(i)?;
    let (i, _) = bytes::take_while(|c: char| c == ' ' || c == '\t')(i)?;
    let (i, ns) = character::i64(i)?;
    let (i, _) = bytes::tag(" ns/iter (+/- ")(i)?;
    let (i, range) = character::i64(i)?;
    let (i, _) = character::char(')')(i)?;
    Ok((i, (ns, range)))
}

/// Parse a bench result.
fn parse_bench_result(i: &str) -> IResult<&str, (&str, i64, i64)> {
    let (i, _) = bytes::take_until("test ")(i)?;
    let (i, name) = parse_bench_result_start(i)?;
    let (i, _) = bytes::take_until("bench:")(i)?;
    let (i, (ns, range)) = parse_bench_result_rest(i)?;
    Ok((i, (name, ns, range)))
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Benchmark {
    pub name: String,
    pub ns: i64,
    pub range: i64,
}

fn parse_benchmarks(i: &str) -> IResult<&str, Vec<Benchmark>> {
    let mut it = combinator::iterator(i, parse_bench_result);
    let parsed = it
        .map(|(name, ns, range)| Benchmark {
            name: name.to_string(),
            ns,
            range,
        })
        .collect::<Vec<_>>();
    let (i, _) = it.finish()?;
    Ok((i, parsed))
}

#[derive(Serialize, Deserialize)]
pub struct HistoricalBenchmark {
    date: DateTime<Utc>,
    benchmark: Benchmark,
    tags: Vec<(String, String)>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct HistoricalBenchmarks {
    benchmarks: Vec<HistoricalBenchmark>,
}

impl HistoricalBenchmarks {
    fn sort(&mut self) {
        self.benchmarks.sort_by(|a, b| a.date.cmp(&b.date));
    }

    fn find_latest_by_name(&self, name: &str) -> Option<&HistoricalBenchmark> {
        for benchmark in self.benchmarks.iter().rev() {
            if benchmark.benchmark.name == name {
                return Some(benchmark);
            }
        }

        None
    }

    fn find_last(&self) -> Option<&HistoricalBenchmark> {
        self.benchmarks.last()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const BENCH_LINE: &'static str =
        "test presize_cubic_cpu/400 ... bench:    17041704 ns/iter (+/- 349284)";

    #[test]
    fn parse_bench() {
        let (i, name) = parse_bench_result_start(BENCH_LINE).unwrap();
        assert_eq!(name, "presize_cubic_cpu/400");

        let (i, (ns, range)) = parse_bench_result_rest(i).unwrap();
        assert_eq!(ns, 17041704);
        assert_eq!(range, 349284);
    }

    #[test]
    fn parse_bench_lines() {
        let lines:String = vec![
            "Gnuplot not found, using plotters backend",
            "test presize_cubic_cpu/400 ... bench:    17041704 ns/iter (+/- 349284)",
            r#"test narrative-maxwell/projects/create ... initializing datadog metrics, writing to stdout: [("service", "maxwell"), ("maxwell.inference.backend", "TFLite"), ("maxwell.cargo.version", "3.2.0"), ("maxwell.git.hash", "7578147d"), ("maxwell.session.id", "65f99ca8-be65-4760-99d8-a15a7bc6b5c5"), ("maxwell.environment", "DEVELOPMENT-STANDALONE")]"#,
            r#"initializing datadog metrics, writing to stdout: [("service", "maxwell"), ("maxwell.inference.backend", "TFLite"), ("maxwell.cargo.version", "3.2.0"), ("maxwell.git.hash", "7578147d"), ("maxwell.session.id", "3822e37c-9106-4d30-805a-35ea2f433b24"), ("maxwell.environment", "DEVELOPMENT-STANDALONE")]"#,
            "bench:     4049764 ns/iter (+/- 3608244)",
        ].join("\n");

        let (i, results) = parse_benchmarks(&lines).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            Benchmark {
                name: "presize_cubic_cpu/400".to_string(),
                ns: 17041704,
                range: 349284
            }
        );
        assert_eq!(
            results[1],
            Benchmark {
                name: "narrative-maxwell/projects/create".to_string(),
                ns: 4049764,
                range: 3608244
            }
        );
    }
}
