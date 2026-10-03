use vstd::prelude::*;
verus! {
pub open spec fn step(x: int) -> int { x + 1 }
pub proof fn lemma_step(x: int) ensures step(x) > x { }
}
