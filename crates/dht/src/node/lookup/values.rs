//! The values (peer endpoints) one get_peers leg or request keeps, shared
//! out between the sources that sent them.

use std::net::SocketAddr;

use super::super::limits::Network;

/// Values one leg, or one request across its legs, keeps.
pub(crate) const MAX_VALUES: usize = 64;

/// At most [`MAX_VALUES`] distinct values, each filed under the source
/// (an IPv4 /24 or an IPv6 /48, as [`super::super::limits::source`] keys
/// it) of the responder that sent it first.
///
/// The rule is max-min fair. While there's room, every value goes in, so
/// a source that answers alone can fill the whole budget. Once it's full,
/// a value from a source holding fewer values than the largest holder
/// takes the place of that holder's newest value; one from the largest
/// holder, or from a source as large, is dropped. So `n` sources that
/// each send enough end up with about `MAX_VALUES / n` each, and a source
/// holding less than that never loses a value.
#[derive(Debug, Default, Clone)]
pub(crate) struct Values {
    /// Per source, its values in arrival order; never an empty list.
    by_source: Vec<(Network, Vec<SocketAddr>)>,
    len: usize,
}

impl Values {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn contains(&self, value: &SocketAddr) -> bool {
        self.iter().any(|v| v == value)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SocketAddr> + '_ {
        self.by_source.iter().flat_map(|(_, vs)| vs.iter())
    }

    /// The values with the source each is filed under.
    pub fn entries(&self) -> impl Iterator<Item = (Network, SocketAddr)> + '_ {
        self.by_source
            .iter()
            .flat_map(|(net, vs)| vs.iter().map(move |v| (*net, *v)))
    }

    /// The values held, counting each source's up to `per_source` only.
    pub fn spread(&self, per_source: usize) -> usize {
        self.by_source
            .iter()
            .map(|(_, vs)| vs.len().min(per_source))
            .sum()
    }

    /// The sources holding values.
    #[cfg(test)]
    pub fn sources(&self) -> usize {
        self.by_source.len()
    }

    fn held(&self, source: Network) -> usize {
        self.by_source
            .iter()
            .find(|(n, _)| *n == source)
            .map_or(0, |(_, vs)| vs.len())
    }

    /// Offers `value` from `source`. Returns whether it was taken (a value
    /// already held, from any source, isn't taken again).
    pub fn insert(&mut self, source: Network, value: SocketAddr) -> bool {
        if self.contains(&value) {
            return false;
        }
        if self.len >= MAX_VALUES {
            let mine = self.held(source);
            // The largest holder; the first of several as large.
            let Some(largest) = self
                .by_source
                .iter()
                .enumerate()
                .filter(|(_, (n, _))| *n != source)
                .max_by(|(i, (_, a)), (j, (_, b))| a.len().cmp(&b.len()).then(j.cmp(i)))
                .map(|(i, _)| i)
            else {
                return false;
            };
            if self.by_source[largest].1.len() <= mine + 1 {
                return false;
            }
            self.by_source[largest].1.pop();
            if self.by_source[largest].1.is_empty() {
                self.by_source.remove(largest);
            }
            self.len -= 1;
        }
        match self.by_source.iter_mut().find(|(n, _)| *n == source) {
            Some((_, vs)) => vs.push(value),
            None => self.by_source.push((source, vec![value])),
        }
        self.len += 1;
        true
    }

    /// Offers every value of `other` under the source it's filed under.
    pub fn merge(&mut self, other: &Values) {
        for (source, value) in other.entries() {
            self.insert(source, value);
        }
    }
}

impl<'a> IntoIterator for &'a Values {
    type Item = &'a SocketAddr;
    type IntoIter = Box<dyn Iterator<Item = &'a SocketAddr> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(b: u8) -> Network {
        Network(4, u64::from(b))
    }

    fn v(net: u8, i: u8) -> SocketAddr {
        SocketAddr::from(([10, net, 0, i], 6881))
    }

    fn fill(vals: &mut Values, source: u8, n: u8) -> usize {
        (0..n)
            .filter(|i| vals.insert(net(source), v(source, *i)))
            .count()
    }

    #[test]
    fn values_budget_is_shared_across_sources() {
        let mut vals = Values::default();
        assert_eq!(fill(&mut vals, 1, 64), 64, "alone, a source fills it");
        assert_eq!(fill(&mut vals, 2, 10), 10, "a second source gets in");
        assert_eq!(vals.len(), MAX_VALUES);
        assert_eq!(vals.held(net(2)), 10);
        assert_eq!(vals.held(net(1)), 54);
        // A third source sending plenty ends level with the first.
        fill(&mut vals, 3, 64);
        assert_eq!(vals.len(), MAX_VALUES);
        assert_eq!(vals.held(net(2)), 10);
        assert_eq!(vals.held(net(1)), 27);
        assert_eq!(vals.held(net(3)), 27);
    }

    #[test]
    fn the_first_values_of_a_source_are_kept() {
        let mut vals = Values::default();
        fill(&mut vals, 1, 64);
        fill(&mut vals, 2, 64);
        assert_eq!(vals.held(net(1)), 32);
        assert!((0..32).all(|i| vals.contains(&v(1, i))));
        assert!(!vals.contains(&v(1, 32)));
        // A full source can't push its own values out, or take more.
        assert!(!vals.insert(net(1), v(1, 200)));
        assert!(!vals.insert(net(2), v(2, 200)));
    }

    /// At an even share (within one), no value moves between sources.
    #[test]
    fn an_even_share_is_stable() {
        let mut vals = Values::default();
        fill(&mut vals, 1, 22);
        fill(&mut vals, 2, 21);
        fill(&mut vals, 3, 21);
        assert_eq!(vals.len(), MAX_VALUES);
        assert!(!vals.insert(net(2), v(2, 100)));
        assert!(!vals.insert(net(3), v(3, 100)));
        assert_eq!(
            (vals.held(net(1)), vals.held(net(2)), vals.held(net(3))),
            (22, 21, 21)
        );
    }

    #[test]
    fn a_value_is_held_once() {
        let mut vals = Values::default();
        assert!(vals.insert(net(1), v(9, 1)));
        assert!(!vals.insert(net(2), v(9, 1)));
        assert_eq!(vals.len(), 1);
        assert_eq!(vals.held(net(2)), 0);
    }

    #[test]
    fn merging_keeps_each_values_source() {
        let mut leg = Values::default();
        fill(&mut leg, 1, 64);
        let mut request = Values::default();
        fill(&mut request, 2, 8);
        request.merge(&leg);
        assert_eq!(request.len(), MAX_VALUES);
        assert_eq!(request.held(net(2)), 8);
        assert_eq!(request.held(net(1)), 56);
    }

    #[test]
    fn spread_counts_each_source_up_to_its_share() {
        let mut vals = Values::default();
        fill(&mut vals, 1, 8);
        assert_eq!(vals.spread(4), 4);
        fill(&mut vals, 2, 1);
        assert_eq!(vals.spread(4), 5);
        fill(&mut vals, 2, 4);
        assert_eq!(vals.spread(4), 8);
        assert_eq!(vals.sources(), 2);
    }

    proptest::proptest! {
        /// Never over the budget, the count always right, each value once,
        /// and a source holding fewer than its fair share never loses one.
        #[test]
        fn the_budget_holds(offers in proptest::collection::vec((0u8..6, 0u8..40), 0..400)) {
            let mut vals = Values::default();
            for (s, i) in offers {
                let before: Vec<usize> = (0..6).map(|n| vals.held(net(n))).collect();
                let mine = before[usize::from(s)];
                vals.insert(net(s), v(s % 3, i));
                proptest::prop_assert!(vals.len() <= MAX_VALUES);
                proptest::prop_assert_eq!(vals.len(), vals.iter().count());
                let mut all: Vec<_> = vals.iter().copied().collect();
                all.sort_unstable();
                all.dedup();
                proptest::prop_assert_eq!(all.len(), vals.len());
                proptest::prop_assert!(vals.by_source.iter().all(|(_, vs)| !vs.is_empty()));
                for (n, was) in (0u8..).zip(before) {
                    let after = vals.held(net(n));
                    if after < was {
                        proptest::prop_assert_eq!(after + 1, was);
                        // Only a source holding at least two more than
                        // the inserting one gives a value up.
                        proptest::prop_assert!(was >= mine + 2);
                        proptest::prop_assert!(after >= MAX_VALUES / 6);
                    }
                }
            }
        }
    }
}
