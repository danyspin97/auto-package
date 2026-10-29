use super::*;

/// Most bytes read from one shared library to look at its dynamic section:
/// `.dynamic` and `.dynstr` sit near the start of an ELF file, far before
/// the code of a library that can be hundreds of megabytes.
const MAX_ELF_BYTES: u64 = 16 * 1024 * 1024;
/// Most shared libraries of one wheel looked at for an `$ORIGIN` RUNPATH.
const MAX_ELF_FILES: usize = 256;

/// The leading bytes of a stream, read on demand and never past
/// [`MAX_ELF_BYTES`], so a parser can ask for any range without the whole
/// (possibly compressed, so not seekable) file being read for it.
struct Prefix<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R: Read> Prefix<R> {
    /// Up to `len` bytes at `off`: fewer when the stream ends first, `None`
    /// when the range starts past its end or past the cap.
    fn upto(&mut self, off: u64, len: u64) -> Option<&[u8]> {
        let end = off.checked_add(len)?.min(MAX_ELF_BYTES);
        let (off, end) = (usize::try_from(off).ok()?, usize::try_from(end).ok()?);
        if let Some(missing) = end.checked_sub(self.buf.len()) {
            (&mut self.reader)
                .take(missing as u64)
                .read_to_end(&mut self.buf)
                .ok()?;
        }
        let end = end.min(self.buf.len());
        self.buf.get(off..end)
    }

    /// Exactly `len` bytes at `off`, or `None`.
    fn exact(&mut self, off: u64, len: u64) -> Option<&[u8]> {
        self.upto(off, len).filter(|b| b.len() as u64 == len)
    }
}

fn le_u16(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le_u32(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from(u32::from_le_bytes(
        b.get(at..at + 4)?.try_into().ok()?,
    )))
}

fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Whether an ELF64 little-endian file has a `DT_RUNPATH` or `DT_RPATH`
/// containing `$ORIGIN`. Reads the program headers, the `PT_DYNAMIC` array and
/// the string table it points at, every range bounds-checked and capped;
/// anything unexpected (not ELF, 32-bit, big-endian, truncated, a bad offset)
/// is `false`.
pub(super) fn elf_has_origin_runpath(reader: impl Read) -> bool {
    elf_origin_runpath(&mut Prefix {
        reader,
        buf: Vec::new(),
    })
    .unwrap_or(false)
}

fn elf_origin_runpath<R: Read>(file: &mut Prefix<R>) -> Option<bool> {
    const PT_LOAD: u64 = 1;
    const PT_DYNAMIC: u64 = 2;
    const PHDR_MIN: u64 = 56;
    const DT_STRTAB: u64 = 5;
    const DT_STRSZ: u64 = 10;
    const DT_RPATH: u64 = 15;
    const DT_RUNPATH: u64 = 29;
    // A library has a handful of program headers and one `.dynamic` of a few
    // dozen entries; these bounds are far above that.
    const MAX_PHNUM: u64 = 256;
    const MAX_DYNAMIC_BYTES: u64 = 64 * 1024;
    const MAX_RUNPATH_BYTES: u64 = 4096;

    let header = file.exact(0, 64)?;
    // ELF magic, ELFCLASS64, ELFDATA2LSB.
    if header[..4] != *b"\x7fELF" || header[4] != 2 || header[5] != 1 {
        return None;
    }
    let (phoff, phentsize, phnum) = (
        le_u64(header, 32)?,
        le_u16(header, 54)?,
        le_u16(header, 56)?,
    );
    if phentsize < PHDR_MIN || phnum == 0 || phnum > MAX_PHNUM {
        return None;
    }
    let phdrs = file.exact(phoff, phentsize * phnum)?;
    // (file offset, virtual address, size in file) per segment.
    let mut loads: Vec<(u64, u64, u64)> = Vec::new();
    let mut dynamic = None;
    for ph in phdrs.chunks_exact(phentsize as usize) {
        let segment = (le_u64(ph, 8)?, le_u64(ph, 16)?, le_u64(ph, 32)?);
        match le_u32(ph, 0)? {
            PT_LOAD => loads.push(segment),
            PT_DYNAMIC => dynamic = Some(segment),
            _ => {}
        }
    }
    let (dyn_off, _, dyn_size) = dynamic?;
    let entries = file.upto(dyn_off, dyn_size.min(MAX_DYNAMIC_BYTES))?;
    let (mut strtab, mut strsz, mut runpaths) = (None, None, Vec::new());
    for entry in entries.chunks_exact(16) {
        let (tag, val) = (le_u64(entry, 0)?, le_u64(entry, 8)?);
        match tag {
            0 => break,
            DT_STRTAB => strtab = Some(val),
            DT_STRSZ => strsz = Some(val),
            DT_RPATH | DT_RUNPATH => runpaths.push(val),
            _ => {}
        }
    }
    if runpaths.is_empty() {
        return Some(false);
    }
    // `DT_STRTAB` is a virtual address: find the segment holding it.
    let strtab = strtab?;
    let strtab_off = loads.iter().find_map(|&(off, vaddr, size)| {
        let rel = strtab.checked_sub(vaddr).filter(|rel| *rel < size)?;
        off.checked_add(rel)
    })?;
    for val in runpaths {
        // The string lies inside the table when its size is known.
        let room = strsz.map_or(MAX_RUNPATH_BYTES, |sz| sz.saturating_sub(val));
        let at = strtab_off.checked_add(val)?;
        let Some(bytes) = file.upto(at, room.min(MAX_RUNPATH_BYTES)) else {
            continue;
        };
        let bytes = bytes.split(|b| *b == 0).next().unwrap_or(&[]);
        if bytes.windows(7).any(|w| w == b"$ORIGIN") {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether any shared library among `names` of the zip has an `$ORIGIN`
/// RUNPATH/RPATH (see [`WheelLayout::origin_runpath`]). Unreadable entries
/// count as not having one.
pub(super) fn any_origin_runpath<R: Read + std::io::Seek>(
    zip_archive: &mut zip::ZipArchive<R>,
    names: &[&str],
) -> bool {
    names.iter().take(MAX_ELF_FILES).any(|name| {
        zip_archive
            .by_name(name)
            .is_ok_and(|entry| entry.size() >= 64 && elf_has_origin_runpath(entry))
    })
}
