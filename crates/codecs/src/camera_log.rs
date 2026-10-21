//! Log profiles that cameras record only in their own metadata, not in the stream's colour tags.
//!
//! Fujifilm (checked on X-M5 HEVC 4:2:0 and 4:2:2 MOVs) tags F-Log / F-Log2 video as BT.709 in
//! the HEVC VUI. Its QuickTime `udta/MVTG` atom carries an EXIF block with a Fujifilm maker note
//! (`FUJIFILM`, a little-endian IFD offset relative to that signature, then the IFD), and
//! maker-note tag `0x3803` says the recording gamma: 0 standard, 16 F-Log, 48 F-Log2. Values we
//! haven't seen on a camera give `None`, so the stream's own tags decide as before.

use filmcraft_color::ColorSpace;

const SIGNATURE: &[u8; 8] = b"FUJIFILM";
/// Maker-note tag holding the video gamma.
const TAG_VIDEO_GAMMA: u16 = 0x3803;
/// TIFF types SHORT and LONG (the X-M5 writes `0x3803` as a LONG).
const SHORT: u16 = 3;
const LONG: u16 = 4;
/// More entries than any real maker note has; a larger count is damage, not a maker note.
const MAX_ENTRIES: usize = 1024;
/// Most `FUJIFILM` occurrences tried (the EXIF `Make` string comes before the maker note).
const MAX_SIGNATURES: usize = 16;

/// The log colour space recorded in a Fujifilm `MVTG` atom's maker note, if it names one.
pub fn fujifilm_mvtg(mvtg: &[u8]) -> Option<ColorSpace> {
    let mut from = 0usize;
    for _ in 0..MAX_SIGNATURES {
        let at = from.checked_add(find(mvtg.get(from..)?, SIGNATURE)?)?;
        if let Some(gamma) = video_gamma(mvtg, at) {
            return match gamma {
                16 => Some(ColorSpace::FLogFGamut),
                48 => Some(ColorSpace::FLog2FGamut),
                _ => None,
            };
        }
        from = at.checked_add(1)?;
    }
    None
}

/// Tag `0x3803` of the maker note whose signature starts at `at`, when that is a well-formed IFD.
fn video_gamma(d: &[u8], at: usize) -> Option<u32> {
    let off = u32_le(d, at.checked_add(SIGNATURE.len())?)? as usize;
    // The IFD starts after the 12-byte header (signature + offset); anything else isn't a maker note.
    if !(12..=64).contains(&off) {
        return None;
    }
    let ifd = at.checked_add(off)?;
    let count = u16_le(d, ifd)? as usize;
    if count == 0 || count > MAX_ENTRIES {
        return None;
    }
    // Every entry must be in bounds, or this isn't the maker note.
    d.get(ifd..ifd.checked_add(2 + count.checked_mul(12)?)?)?;
    (0..count).find_map(|i| {
        let e = ifd + 2 + i * 12;
        if u16_le(d, e)? != TAG_VIDEO_GAMMA || u32_le(d, e + 4)? != 1 {
            return None;
        }
        match u16_le(d, e + 2)? {
            SHORT => u16_le(d, e + 8).map(u32::from),
            LONG => u32_le(d, e + 8),
            _ => None,
        }
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn u16_le(d: &[u8], at: usize) -> Option<u16> {
    let b = d.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([*b.first()?, *b.get(1)?]))
}

fn u32_le(d: &[u8], at: usize) -> Option<u32> {
    let b = d.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `MVTG`-like block: a header, the EXIF `Make` string, then a maker note whose IFD has
    /// `0x1031` (SHORT) and `0x3803 = gamma` (LONG), as in X-M5 files.
    fn mvtg(gamma: u32) -> Vec<u8> {
        let mut d = vec![0, 0, 0, 0, 0, 0, 0, 1];
        d.extend_from_slice(b"FUJIFILM\0\0X-M5\0\0");
        d.extend_from_slice(SIGNATURE);
        d.extend_from_slice(&12u32.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        for (tag, ty, v) in [(0x1031u16, SHORT, 768u32), (TAG_VIDEO_GAMMA, LONG, gamma)] {
            d.extend_from_slice(&tag.to_le_bytes());
            d.extend_from_slice(&ty.to_le_bytes());
            d.extend_from_slice(&1u32.to_le_bytes());
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&[0; 4]);
        d
    }

    #[test]
    fn reads_the_fujifilm_video_gamma() {
        assert_eq!(fujifilm_mvtg(&mvtg(16)), Some(ColorSpace::FLogFGamut));
        assert_eq!(fujifilm_mvtg(&mvtg(48)), Some(ColorSpace::FLog2FGamut));
        assert_eq!(fujifilm_mvtg(&mvtg(0)), None, "standard: the stream's tags decide");
        assert_eq!(fujifilm_mvtg(&mvtg(32)), None, "a value no camera was seen to write");
        // a SHORT entry (other bodies or firmware) reads the same
        let mut short = mvtg(48);
        let e = short.len() - 4 - 12;
        short[e + 2] = SHORT as u8;
        assert_eq!(fujifilm_mvtg(&short), Some(ColorSpace::FLog2FGamut));
        assert_eq!(fujifilm_mvtg(b""), None);
        assert_eq!(fujifilm_mvtg(b"FUJIFILM"), None);
    }

    #[test]
    fn a_maker_note_without_the_tag_says_nothing() {
        let mut d = mvtg(48);
        let at = d.len() - 4 - 12;
        d[at] = 0x04; // 0x3803 → 0x3804
        assert_eq!(fujifilm_mvtg(&d), None);
    }

    /// Real camera files, when `FILMCRAFT_FUJI_DIR` names a folder of them (never committed):
    /// each file whose name says `f-log2` / `f-log` / `std` must probe as that profile; other
    /// names are only opened (no panic, and anything detected is a Fujifilm log space).
    #[test]
    fn real_fujifilm_files_when_provided() {
        let Some(dir) = std::env::var_os("FILMCRAFT_FUJI_DIR") else { return };
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        for e in entries.flatten() {
            let path = e.path();
            let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !name.ends_with(".mov") {
                continue;
            }
            let src = filmcraft_media::open_path(&path, &crate::openers()).unwrap_or_else(|err| panic!("{name}: {err}"));
            let got = src.info().video.as_ref().and_then(|v| v.camera_color_space);
            let want = if name.contains("f-log2") {
                Some(Some(ColorSpace::FLog2FGamut))
            } else if name.contains("f-log") {
                Some(Some(ColorSpace::FLogFGamut))
            } else if name.contains("std") {
                Some(None)
            } else {
                None
            };
            match want {
                Some(w) => assert_eq!(got, w, "{name}"),
                None => assert!(matches!(got, None | Some(ColorSpace::FLogFGamut | ColorSpace::FLog2FGamut)), "{name}: {got:?}"),
            }
            eprintln!("{name}: {got:?}");
        }
    }

    #[test]
    fn hostile_bytes_never_panic() {
        let good = mvtg(48);
        for n in 0..good.len() {
            let _ = fujifilm_mvtg(&good[..n]);
        }
        for i in 0..good.len() {
            for bit in 0..8 {
                let mut d = good.clone();
                d[i] ^= 1 << bit;
                let _ = fujifilm_mvtg(&d);
            }
        }
        let mut huge = SIGNATURE.to_vec();
        huge.extend_from_slice(&12u32.to_le_bytes());
        huge.extend_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(fujifilm_mvtg(&huge), None);
        assert_eq!(fujifilm_mvtg(&SIGNATURE.repeat(100)), None);
    }
}
