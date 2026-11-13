//! Parallel helpers: rayon with the `parallel` feature, plain loops without it. Callers only
//! combine results in input order, so the output is identical either way.

#[cfg(feature = "parallel")]
pub(crate) fn join<A, B, RA, RB>(a: A, b: B) -> (RA, RB)
where
    A: FnOnce() -> RA + Send,
    B: FnOnce() -> RB + Send,
    RA: Send,
    RB: Send,
{
    rayon::join(a, b)
}

#[cfg(not(feature = "parallel"))]
pub(crate) fn join<A, B, RA, RB>(a: A, b: B) -> (RA, RB)
where
    A: FnOnce() -> RA,
    B: FnOnce() -> RB,
{
    (a(), b())
}

/// `items.map(f)` in input order, in parallel batches of at least `min_len`.
#[cfg(feature = "parallel")]
pub(crate) fn map<T: Sync, R: Send>(items: &[T], min_len: usize, f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
    use rayon::prelude::*;
    items.par_iter().with_min_len(min_len.max(1)).map(f).collect()
}

#[cfg(not(feature = "parallel"))]
pub(crate) fn map<T, R>(items: &[T], _min_len: usize, f: impl Fn(&T) -> R) -> Vec<R> {
    items.iter().map(f).collect()
}
