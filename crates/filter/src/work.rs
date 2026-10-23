//! A count of the steps the matching loops take on untrusted text, so
//! tests can hold hostile names to a bound on work rather than on time
//! (§6 "Hostile-input limits"). Outside tests it counts nothing and
//! compiles away.

/// Counts `n` steps of work on this thread.
#[inline(always)]
pub(crate) fn tick(n: usize) {
    #[cfg(test)]
    COUNT.with(|c| c.set(c.get() + n as u64));
    #[cfg(not(test))]
    let _ = n;
}

#[cfg(test)]
thread_local! {
    static COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The steps `f` took on this thread.
#[cfg(test)]
pub(crate) fn counted<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = COUNT.with(|c| c.get());
    let out = f();
    (out, COUNT.with(|c| c.get()) - before)
}
