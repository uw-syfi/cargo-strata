#[path = "../../vendor/evil.rs"]
mod evil;
pub use effects::{Foo, device};
pub use effects::*;
use effects::Private;
const DATA: &str = include_str!("../../vendor/data.txt");
const OK: &str = include_str!("data.txt");

pub fn build() {
    let _a = gate::Permit { id: 2 };
    let _b = gate::permit(3);
    let _c = vec![Permit { id: 4 }];
    let _d = Permit::new(5);
    let _f = other.permit();
    let Permit { id } = _a;
    unsafe { std::hint::unreachable_unchecked() }
}

fn control() {
    let _ = Permit { id: 6 }; // negctl: fails here
}

fn control2() {
    // negctl: fails here
    let _ = Permit { id: 6 };
}

unsafe fn raw() {}
unsafe impl Send for S {}
struct S;

#[cfg(test)]
mod tests {
    fn t() {
        let _ = Permit { id: 9 };
    }
}

// Permit { id: 1 } and unsafe { } in a comment
const TEXT: &str = "Permit { id: 1 } unsafe fn";
