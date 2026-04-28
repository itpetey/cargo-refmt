#[derive(Clone)]
pub struct PublicStruct {
    x: i32,
}

pub enum PublicEnum {
    A,
    B,
}

struct PrivateStruct {
    y: i32,
}

struct PrivateEnum {
    x: i32,
}
