//! A reader that caps the bodies of the metadata entries of a tar stream: a
//! GNU long name or long link, and a PAX header. The tar reader holds such a
//! body in memory whole, so a hostile archive could make it allocate as much
//! as the stream holds. This reader passes every byte through unchanged,
//! follows the 512-byte header blocks the tar reader reads, and fails before
//! the body of an oversized metadata entry is read.

use std::io::{self, Read};

/// The largest body a metadata entry may have. Real long names and PAX
/// headers are a few hundred bytes.
pub(super) const MAX_META: u64 = 256 * 1024;

const BLOCK: usize = 512;

pub(super) struct MetaGuard<R> {
    inner: R,
    state: State,
}

enum State {
    /// Reading a header block: `filled` of its bytes are in `block`.
    Header { block: [u8; BLOCK], filled: usize },
    /// Inside an entry's body and its padding: `left` bytes to go.
    Body { left: u64 },
}

fn header_state() -> State {
    State::Header {
        block: [0; BLOCK],
        filled: 0,
    }
}

impl<R: Read> MetaGuard<R> {
    pub(super) fn new(inner: R) -> Self {
        MetaGuard {
            inner,
            state: header_state(),
        }
    }
}

impl<R: Read> Read for MetaGuard<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let (n, next) = match &mut self.state {
            State::Body { left } => {
                let want = (*left).min(out.len() as u64) as usize;
                let n = self.inner.read(&mut out[..want])?;
                *left -= n as u64;
                (n, (*left == 0).then(header_state))
            }
            State::Header { block, filled } => {
                let want = (BLOCK - *filled).min(out.len());
                let n = self.inner.read(&mut out[..want])?;
                block[*filled..*filled + n].copy_from_slice(&out[..n]);
                *filled += n;
                if *filled < BLOCK {
                    (n, None)
                } else {
                    (n, Some(after_header(block)?))
                }
            }
        };
        if let Some(state) = next {
            self.state = state;
        }
        Ok(n)
    }
}

/// The state after a header block: the body it announces, or the next header
/// when it has none. Refuses a metadata body over [`MAX_META`].
fn after_header(block: &[u8; BLOCK]) -> io::Result<State> {
    let header = tar::Header::from_byte_slice(block);
    // A block whose size is not a number is the end of the archive or the
    // tar reader's own error to report. Neither has a body to cap here.
    let Ok(size) = header.size() else {
        return Ok(header_state());
    };
    let kind = header.entry_type();
    let metadata = kind.is_gnu_longname()
        || kind.is_gnu_longlink()
        || kind.is_pax_local_extensions()
        || kind.is_pax_global_extensions();
    if metadata && size > MAX_META {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a tar metadata entry of {size} bytes is over the {MAX_META}-byte limit"),
        ));
    }
    // The body is padded to whole blocks.
    let padded = size.saturating_add(BLOCK as u64 - 1) / BLOCK as u64 * BLOCK as u64;
    Ok(if padded == 0 {
        header_state()
    } else {
        State::Body { left: padded }
    })
}
