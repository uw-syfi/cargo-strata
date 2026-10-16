// The one file allowed to build a Permit.
pub struct Permit { pub id: u32 }
pub fn mint() -> Permit {
    Permit { id: 1 }
}
pub fn permit(x: u32) -> Permit {
    Permit { id: x }
}
// LINK: the C entry point
#[no_mangle]
pub extern "C" fn gate_entry() {}
#[unsafe(no_mangle)]
pub extern "C" fn untagged() {}
