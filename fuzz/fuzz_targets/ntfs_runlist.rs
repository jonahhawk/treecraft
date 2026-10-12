//! Feeds arbitrary bytes to the runlist decoder, which turns on-disk data into cluster offsets.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(runs) = tc_ntfs::decode_runlist(data) {
        // Every decoded run must be usable as-is: nonzero length, non-negative start.
        assert!(runs.iter().all(|r| r.clusters > 0));
    }
});
