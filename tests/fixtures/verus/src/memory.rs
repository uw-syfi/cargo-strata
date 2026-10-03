use vstd::prelude::*;
verus! {
use crate::util::double;                       // ok
use crate::engine::step;                       // VIOLATION

pub open spec fn m(x: int) -> int { double(x) }

// signature mentions engine
pub open spec fn bad_sig(x: int) -> bool
    recommends crate::engine::step(x) > 0,     // VIOLATION
{
    true
}

proof fn lemma(x: int)
    requires x > 0,
    ensures super::util::double(x) > 0,        // ok
{
    assert(crate::engine::step(x) > x) by {    // VIOLATION
        crate::engine::lemma_step(x);          // VIOLATION
    }
}

exec fn run(x: u64) -> (r: u64)
    requires x < 100,
    ensures r == x,
{
    let y = x;
    let _z: Option<crate::engine::Nothing> = None; // VIOLATION
    y
}
}
