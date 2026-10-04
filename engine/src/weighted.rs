//! Weighted random choices, their tables built as the pack loads.

use rand::Rng;
use rand::distr::Distribution;
use rand::distr::weighted::WeightedIndex;

use crate::pack::Error;

#[derive(Debug)]
pub struct Weighted<T> {
    index: WeightedIndex<u32>,
    items: Vec<T>,
}

impl<T> Weighted<T> {
    pub fn new(entries: impl IntoIterator<Item = (u32, T)>) -> Result<Weighted<T>, Error> {
        let (weights, items): (Vec<u32>, Vec<T>) = entries.into_iter().unzip();
        let index = WeightedIndex::new(&weights).map_err(|e| Error(format!("weights {weights:?}: {e}")))?;
        Ok(Weighted { index, items })
    }

    pub fn pick(&self, rng: &mut impl Rng) -> &T {
        &self.items[self.index.sample(rng)]
    }
}
