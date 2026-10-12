//! Feeds arbitrary bytes to the FILE record parser and everything the MFT scanner calls on a
//! record: fixups, attribute iteration, the summary, runlists and attribute lists.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tc_ntfs::{
    BootSector, Body, FileRecord, FileSummary, apply_fixups, attr, decode_runlist,
    parse_attribute_list,
};

fuzz_target!(|data: &[u8]| {
    let _ = BootSector::parse(data);
    let mut record = data.to_vec();
    let _ = apply_fixups(&mut record);
    let Ok(r) = FileRecord::parse(&record) else {
        return;
    };
    let _ = FileSummary::default().add_record(&r);
    for a in r.attributes().flatten() {
        match a.body {
            Body::NonResident(n) => {
                let _ = decode_runlist(n.runlist);
            }
            Body::Resident(v) if a.type_code == attr::ATTRIBUTE_LIST => {
                let _ = parse_attribute_list(v);
            }
            Body::Resident(_) => {}
        }
    }
});
