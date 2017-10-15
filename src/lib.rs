#![unstable(feature = "alloc",
            reason = "this library is unlikely to be stabilized in its current \
                      form or name",
            issue = "27783")]

#![feature(core_intrinsics)]
#![feature(staged_api)]
#![feature(unique)]

#![no_std]

pub mod allocator;
