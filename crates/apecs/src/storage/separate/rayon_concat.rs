use rayon::{iter::plumbing::ProducerCallback, prelude::*};

pub struct Concat<I>(pub Vec<I>);

impl<I: IndexedParallelIterator> ParallelIterator for Concat<I> {
    type Item = I::Item;

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: rayon::iter::plumbing::UnindexedConsumer<Self::Item>,
    {
        rayon::iter::plumbing::bridge(self, consumer)
    }
}

impl<I: IndexedParallelIterator> IndexedParallelIterator for Concat<I> {
    fn len(&self) -> usize {
        self.0.iter().map(IndexedParallelIterator::len).sum()
    }

    fn drive<C: rayon::iter::plumbing::Consumer<Self::Item>>(mut self, consumer: C) -> C::Result {
        use rayon::iter::plumbing::Reducer;
        println!("drive self.len(): {}", self.len());
        let len = self.len();
        if let Some(tail) = self.0.pop() {
            println!("  tail.len: {}", tail.len());
            println!("  len - tail.len: {}", len - tail.len());
            let (left, right, reducer) = consumer.split_at(len - tail.len());
            let (a, b) = rayon::join(|| self.drive(left), || tail.drive(right));
            reducer.reduce(a, b)
        } else {
            rayon::iter::empty().drive(consumer)
        }
    }

    fn with_producer<CB: ProducerCallback<Self::Item>>(mut self, callback: CB) -> CB::Output {
        return callback.callback(ConcatProducer(vec![], self.0, vec![]));

        struct ConcatProducer<T: IndexedParallelIterator>(Vec<T::Item>, Vec<T>, Vec<T::Item>);

        impl<T: IndexedParallelIterator> Default for ConcatProducer<T> {
            fn default() -> Self {
                Self(vec![], vec![], vec![])
            }
        }

        impl<T: IndexedParallelIterator> ConcatProducer<T> {
            fn total_len(&self) -> usize {
                self.0.len()
                    + self
                        .1
                        .iter()
                        .map(IndexedParallelIterator::len)
                        .sum::<usize>()
                    + self.2.len()
            }
        }

        impl<T: IndexedParallelIterator> rayon::iter::plumbing::Producer for ConcatProducer<T> {
            type Item = T::Item;

            type IntoIter = std::vec::IntoIter<Self::Item>;

            fn into_iter(self) -> Self::IntoIter {
                let mut vs: Vec<Self::Item> = self.0;
                vs.extend(self.1.into_par_iter().flatten().collect::<Vec<_>>());
                vs.extend(self.2);
                vs.into_iter()
            }

            fn split_at(mut self, index: usize) -> (Self, Self) {
                let mut lines = vec![];
                //lines.push(format!(
                //    "split_at: index:{} left:{}  center:{},{}  right:{}",
                //    index,
                //    self.0.len(),
                //    self.1.len(),
                //    self.1
                //        .iter()
                //        .map(IndexedParallelIterator::len)
                //        .sum::<usize>(),
                //    self.2.len()
                //));
                let total_len = self.total_len();
                let mut left = ConcatProducer::<T>::default();
                let taken_from_left = self
                    .0
                    .splice(0..index.min(self.0.len()), std::iter::empty())
                    .collect::<Vec<_>>();
                left.0 = taken_from_left;
                if !left.0.is_empty() {
                    lines.push(format!("  took {} from left", left.0.len()));
                }

                let (mut left, mut right) = std::mem::take(&mut self.1).into_iter().fold(
                    (left, self),
                    |(mut left, mut right), vs| {
                        let left_len = left.total_len();
                        let iter_len = vs.len();
                        if left_len + iter_len < index {
                            //lines.push(format!("    {} + {} < {} - going left", left_len, iter_len, index));
                            // we have not yet accumulated enough for the split,
                            // everything goes in left
                            left.1.push(vs);
                        } else if left_len >= index {
                            //lines.push(format!("    {} >= {} - going right", left_len, index));
                            // we already meet our split quota, everything goes in right
                            right.1.push(vs);
                        } else {
                            //lines.push(format!(
                            //    "    index:{} splitting with splice 0..{}",
                            //    index,
                            //    index - left_len
                            //));
                            // we have to split this vec
                            let mut vs = vs.into_par_iter().collect::<Vec<_>>();
                            let vs_left = vs.splice(0..(index - left_len), std::iter::empty());
                            left.2.extend(vs_left);
                            right.0.extend(vs);
                        }
                        (left, right)
                    },
                );

                let left_len = left.total_len();
                if left_len < index {
                    left.2
                        .extend(right.2.splice(0..(index - left_len), std::iter::empty()));
                }

                //println!("{}", lines.join("\n"));
                assert_eq!(left.total_len(), index, "{} /= {}", left.total_len(), index);
                assert_eq!(
                    left.total_len() + right.total_len(),
                    total_len,
                    "{} + {} /= {}",
                    left.total_len(),
                    right.total_len(),
                    total_len
                );
                (left, right)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use rayon::iter::IntoParallelIterator;

    use super::*;

    #[test]
    fn parallel_concat_sanity() {
        let x = 1000;
        let y = 1000;
        let vs = vec![vec![1.0f32; x]; y];

        let sum: f32 = Concat(
            vs.into_iter()
                .map(IntoParallelIterator::into_par_iter)
                .collect(),
        )
        .sum();
        assert_eq!((x * y) as f32, sum);
    }
}
