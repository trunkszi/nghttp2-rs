//! Unit scenarios for terminal DATA chunks split by the native read limit.
use std::{cell::Cell, rc::Rc};

use super::*;

struct FinalSource {
    bytes: Vec<u8>,
    polls: Rc<Cell<usize>>,
    mode: u8,
}
impl DataSource for FinalSource {
    fn read(&mut self, _: usize) -> DataPoll<'_> {
        self.polls.set(self.polls.get() + 1);
        match self.mode {
            0 => DataPoll::CopyEof(&self.bytes),
            1 => DataPoll::CopyEofWithTrailers(&self.bytes),
            2 => DataPoll::NoCopyEof(&self.bytes),
            _ => DataPoll::NoCopyEofWithTrailers(&self.bytes),
        }
    }
}
#[test]
fn unit_scenario_final_copy_and_no_copy_chunks_only_mark_the_last_split_terminal() {
    for mode in 0..4 {
        for size in [0usize, 1, 8, 25] {
            let polls = Rc::new(Cell::new(0));
            let provider = DataProvider::new(FinalSource {
                bytes: vec![0x5a; size],
                polls: polls.clone(),
                mode,
            });
            // SAFETY: provider owns this descriptor and its stable source allocation.
            let mut raw = unsafe { *provider.as_raw() };
            let mut received = Vec::new();
            for index in 0..size.div_ceil(8).max(1) {
                let mut output = [0u8; 8];
                let mut flags = 0;
                // SAFETY: writable output/flags/source; provider stays live, and the
                // trampoline does not dereference the unused session/userdata arguments.
                let length = unsafe {
                    raw.read_callback.expect("provider callback")(
                        ptr::null_mut(),
                        1,
                        output.as_mut_ptr(),
                        output.len(),
                        &mut flags,
                        &mut raw.source,
                        ptr::null_mut(),
                    )
                };
                assert!(length >= 0);
                let length = length as usize;
                if mode >= 2 && length > 0 {
                    assert_ne!(flags & NGHTTP2_DATA_FLAG_NO_COPY, 0);
                    // SAFETY: this matches the completed NO_COPY read on the same live provider.
                    let (bytes, count) = unsafe { no_copy_chunk(&mut raw.source) }.expect("chunk");
                    assert_eq!(count, length);
                    // SAFETY: the source owns count readable bytes through this simulated send.
                    received.extend_from_slice(unsafe { std::slice::from_raw_parts(bytes, count) });
                } else {
                    assert_eq!(flags & NGHTTP2_DATA_FLAG_NO_COPY, 0);
                    received.extend_from_slice(&output[..length]);
                }
                let last = (index + 1) * 8 >= size;
                assert_eq!(flags & NGHTTP2_DATA_FLAG_EOF != 0, last);
                assert_eq!(
                    flags & NGHTTP2_DATA_FLAG_NO_END_STREAM != 0,
                    last && mode % 2 == 1
                );
            }
            assert_eq!(received, vec![0x5a; size]);
            assert_eq!(
                polls.get(),
                1,
                "pending splits must not poll their source again"
            );
        }
    }
}
