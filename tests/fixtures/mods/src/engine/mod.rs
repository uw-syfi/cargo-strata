pub struct E;
use crate::memory::Public; // allowed: engine has no depends_on
use crate::memory::private::P; // VIOLATION line 3 (deny)
