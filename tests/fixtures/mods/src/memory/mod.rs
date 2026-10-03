pub mod private;
pub struct Public;
use crate::util::U; // ok
use self::private::P; // ok, own subtree
use crate::engine::E; // VIOLATION line 5
use crate::{util::U as U2, engine::{E as E2, self}}; // VIOLATION line 6 (twice, same line)
pub fn f() -> usize {
    let _ = super::engine::E; // VIOLATION line 8
    let _: Option<crate::engine::E> = None; // VIOLATION line 9
    vec![crate::engine::E].len() // VIOLATION line 10 (inside macro tokens)
}
mod inline {
    use super::super::engine::E; // VIOLATION line 13
    use super::P2; // ok
}
#[cfg(test)]
mod tests {
    use crate::engine::E; // skipped: cfg(test)
}
pub struct P2;
