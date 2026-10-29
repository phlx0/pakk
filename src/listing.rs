use std::time::SystemTime;

use crate::walk::Kind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub name: String,
    pub kind: Kind,
    pub mtime: Option<SystemTime>,
}
