//! Operating-system boundary. All `unsafe` in this crate lives below this module.

mod darwin;

pub(crate) use darwin::*;
