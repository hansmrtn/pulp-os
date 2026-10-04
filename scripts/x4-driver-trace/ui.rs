#[derive(Clone, Copy, Debug)]
pub struct Region { pub x: u16, pub y: u16, pub w: u16, pub h: u16 }
impl Region { pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self { Self { x, y, w, h } } }
