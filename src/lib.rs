#![unstable(feature = "alloc",
            reason = "this library is unlikely to be stabilized in its current \
                      form or name",
            issue = "27783")]

#![feature(core_intrinsics)]
#![feature(rustc_attrs)]
#![feature(staged_api)]
#![feature(test)]
#![feature(unique)]

#![no_std]

pub mod allocator;
pub mod boxed;
pub mod heap;
