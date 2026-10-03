use target_crate::api::X;
use target_crate::inner::Y;
use target_crate::{root_item, shapes::Z};

pub fn f() -> (X, Z) {
    root_item();
    let _y = target_crate::inner::Y;
    target_crate::api::X;
    (X, Z)
}
