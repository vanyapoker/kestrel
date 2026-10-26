//! Camera details from a raw Exif block (a TIFF structure): camera, lens, when the
//! photo was taken, exposure settings and GPS position.

/// Info rows for the preview panel, in display order.
pub fn rows(chunk: &[u8]) -> Vec<(String, String)> {
    let Some(tiff) = Tiff::new(chunk) else {
        return Vec::new();
    };
    let Some(ifd0) = tiff.u32(4).and_then(|at| tiff.ifd(at as usize)) else {
        return Vec::new();
    };
    let sub = |tag| {
        ifd0.iter()
            .find(|f| f.tag == tag)
            .and_then(|f| tiff.uint(f))
            .and_then(|at| tiff.ifd(at as usize))
            .unwrap_or_default()
    };
    let (exif, gps) = (sub(0x8769), sub(0x8825));
    let find = |ifd: &[Field], tag| ifd.iter().find(|f| f.tag == tag).copied();
    let text = |ifd: &[Field], tag| find(ifd, tag).and_then(|f| tiff.ascii(&f));
    let number = |ifd: &[Field], tag| {
        find(ifd, tag)
            .and_then(|f| tiff.rationals(&f).first().copied())
            .filter(|v| v.is_finite() && *v > 0.0)
    };

    let mut rows = Vec::new();
    let make = text(&ifd0, 0x010F).unwrap_or_default();
    let model = text(&ifd0, 0x0110).unwrap_or_default();
    // Models usually repeat the brand ("Canon" + "Canon EOS R5").
    let camera = if model.to_lowercase().starts_with(&make.to_lowercase()) {
        model
    } else {
        format!("{make} {model}").trim().to_string()
    };
    if !camera.is_empty() {
        rows.push(("Camera".into(), camera));
    }
    if let Some(lens) = text(&exif, 0xA434) {
        rows.push(("Lens".into(), lens));
    }
    if let Some(taken) = text(&exif, 0x9003).or_else(|| text(&ifd0, 0x0132)) {
        rows.push(("Taken".into(), format_date(&taken)));
    }

    let mut exposure = Vec::new();
    if let Some(t) = number(&exif, 0x829A) {
        exposure.push(if t < 0.5 {
            format!("1/{} s", (1.0 / t).round())
        } else {
            format!("{} s", trim_float(t, 1))
        });
    }
    if let Some(f) = number(&exif, 0x829D) {
        exposure.push(format!("f/{}", trim_float(f, 1)));
    }
    if let Some(iso) = find(&exif, 0x8827).and_then(|f| tiff.uint(&f)) {
        exposure.push(format!("ISO {iso}"));
    }
    if let Some(mm) = number(&exif, 0x920A) {
        let equiv = find(&exif, 0xA405)
            .and_then(|f| tiff.uint(&f))
            .filter(|&e| e > 0 && e != mm.round() as u32);
        exposure.push(match equiv {
            Some(e) => format!("{} mm ({e} mm equiv.)", trim_float(mm, 1)),
            None => format!("{} mm", trim_float(mm, 1)),
        });
    }
    if !exposure.is_empty() {
        rows.push(("Exposure".into(), exposure.join(" · ")));
    }

    let coord = |value_tag, ref_tag, negative: &str| {
        let parts = tiff.rationals(&find(&gps, value_tag)?);
        let [d, m, s] = parts[..] else { return None };
        let value = d + m / 60.0 + s / 3600.0;
        let sign = if text(&gps, ref_tag)?.eq_ignore_ascii_case(negative) {
            -1.0
        } else {
            1.0
        };
        value.is_finite().then_some(sign * value)
    };
    if let (Some(lat), Some(lon)) = (coord(0x0002, 0x0001, "S"), coord(0x0004, 0x0003, "W")) {
        rows.push(("Location".into(), format!("{lat:.5}, {lon:.5}")));
    }
    rows
}

/// "2024:05:01 14:22:10" → "2024-05-01 14:22", matching how file dates are shown.
fn format_date(raw: &str) -> String {
    match raw.split_once(' ') {
        Some((date, time)) if date.len() == 10 && time.len() >= 5 => {
            format!("{} {}", date.replace(':', "-"), &time[..5])
        }
        _ => raw.to_string(),
    }
}

/// `2.0` → "2", `2.8` → "2.8".
fn trim_float(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

#[derive(Clone, Copy)]
struct Field {
    tag: u16,
    kind: u16,
    count: u32,
    /// Where the value is stored: inline in the entry, or at an offset.
    at: usize,
}

struct Tiff<'a> {
    data: &'a [u8],
    little: bool,
}

impl<'a> Tiff<'a> {
    fn new(chunk: &'a [u8]) -> Option<Self> {
        let data = chunk.strip_prefix(b"Exif\0\0").unwrap_or(chunk);
        let little = match data.get(..4)? {
            [b'I', b'I', 42, 0] => true,
            [b'M', b'M', 0, 42] => false,
            _ => return None,
        };
        Some(Self { data, little })
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// The entries of the directory at `at`.
    fn ifd(&self, at: usize) -> Option<Vec<Field>> {
        let n = self.u16(at)? as usize;
        // Real directories hold a few dozen entries; more means a corrupt offset.
        if n > 1024 {
            return None;
        }
        let fields = (0..n)
            .filter_map(|i| {
                let entry = at + 2 + i * 12;
                let kind = self.u16(entry + 2)?;
                let count = self.u32(entry + 4)?;
                let unit = match kind {
                    1 | 2 | 6 | 7 => 1,
                    3 | 8 => 2,
                    4 | 9 | 11 => 4,
                    5 | 10 | 12 => 8,
                    _ => return None,
                };
                let bytes = (count as usize).checked_mul(unit)?;
                let at = if bytes <= 4 {
                    entry + 8
                } else {
                    self.u32(entry + 8)? as usize
                };
                self.data.get(at..at.checked_add(bytes)?)?;
                Some(Field {
                    tag: self.u16(entry)?,
                    kind,
                    count,
                    at,
                })
            })
            .collect();
        Some(fields)
    }

    fn ascii(&self, f: &Field) -> Option<String> {
        if f.kind != 2 {
            return None;
        }
        let raw = self.data.get(f.at..f.at + f.count as usize)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let s = String::from_utf8_lossy(&raw[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn uint(&self, f: &Field) -> Option<u32> {
        match f.kind {
            3 => self.u16(f.at).map(u32::from),
            4 => self.u32(f.at),
            _ => None,
        }
    }

    fn rationals(&self, f: &Field) -> Vec<f64> {
        if !matches!(f.kind, 5 | 10) {
            return Vec::new();
        }
        (0..f.count.min(16) as usize)
            .filter_map(|i| {
                let (n, d) = (self.u32(f.at + i * 8)?, self.u32(f.at + i * 8 + 4)?);
                let (n, d) = if f.kind == 10 {
                    (f64::from(n as i32), f64::from(d as i32))
                } else {
                    (f64::from(n), f64::from(d))
                };
                (d != 0.0).then_some(n / d)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a TIFF block in either byte order from (tag, type, count, payload) entries.
    struct Builder {
        little: bool,
        data: Vec<u8>,
    }

    type Entry = (u16, u16, u32, Vec<u8>);

    impl Builder {
        fn new(little: bool) -> Self {
            let mut data = if little {
                b"II*\0".to_vec()
            } else {
                b"MM\0*".to_vec()
            };
            data.extend([0; 4]);
            Self { little, data }
        }
        fn u16(&self, v: u16) -> [u8; 2] {
            if self.little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        }
        fn u32(&self, v: u32) -> [u8; 4] {
            if self.little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        }
        fn ascii(s: &str) -> (u16, u32, Vec<u8>) {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            (2, b.len() as u32, b)
        }
        fn rational(&self, parts: &[(u32, u32)]) -> (u16, u32, Vec<u8>) {
            let b = parts
                .iter()
                .flat_map(|&(n, d)| [self.u32(n), self.u32(d)].concat())
                .collect();
            (5, parts.len() as u32, b)
        }
        fn short(&self, v: u16) -> (u16, u32, Vec<u8>) {
            (3, 1, self.u16(v).to_vec())
        }
        /// Appends a directory and returns its offset.
        fn ifd(&mut self, entries: &[Entry]) -> u32 {
            let at = self.data.len();
            let mut extra = at + 2 + entries.len() * 12 + 4;
            let mut tail: Vec<u8> = Vec::new();
            let mut head = self.u16(entries.len() as u16).to_vec();
            for (tag, kind, count, payload) in entries {
                head.extend(self.u16(*tag));
                head.extend(self.u16(*kind));
                head.extend(self.u32(*count));
                if payload.len() <= 4 {
                    let mut inline = payload.clone();
                    inline.resize(4, 0);
                    head.extend(inline);
                } else {
                    head.extend(self.u32(extra as u32));
                    tail.extend(payload);
                    extra += payload.len();
                }
            }
            head.extend([0; 4]);
            self.data.extend(head);
            self.data.extend(tail);
            at as u32
        }
        fn entry(tag: u16, (kind, count, payload): (u16, u32, Vec<u8>)) -> Entry {
            (tag, kind, count, payload)
        }
    }

    fn sample(little: bool) -> Vec<u8> {
        let mut b = Builder::new(little);
        let exif = vec![
            Builder::entry(0x829A, b.rational(&[(1, 250)])),
            Builder::entry(0x829D, b.rational(&[(28, 10)])),
            Builder::entry(0x8827, b.short(400)),
            Builder::entry(0x920A, b.rational(&[(42, 10)])),
            Builder::entry(0xA405, b.short(26)),
            Builder::entry(0x9003, Builder::ascii("2024:05:01 14:22:10")),
            Builder::entry(0xA434, Builder::ascii("Wide Camera")),
        ];
        let exif_at = b.ifd(&exif);
        let gps = vec![
            Builder::entry(0x0001, Builder::ascii("N")),
            Builder::entry(0x0002, b.rational(&[(48, 1), (51, 1), (3024, 100)])),
            Builder::entry(0x0003, Builder::ascii("W")),
            Builder::entry(0x0004, b.rational(&[(2, 1), (17, 1), (4020, 100)])),
        ];
        let gps_at = b.ifd(&gps);
        let ifd0 = vec![
            Builder::entry(0x010F, Builder::ascii("Apple")),
            Builder::entry(0x0110, Builder::ascii("iPhone 15")),
            (0x8769, 4, 1, b.u32(exif_at).to_vec()),
            (0x8825, 4, 1, b.u32(gps_at).to_vec()),
        ];
        let ifd0_at = b.ifd(&ifd0);
        let first = b.u32(ifd0_at);
        b.data[4..8].copy_from_slice(&first);
        b.data
    }

    #[test]
    fn reads_camera_details_in_both_byte_orders() {
        for little in [true, false] {
            let rows = rows(&sample(little));
            let get = |k: &str| {
                rows.iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.as_str())
            };
            assert_eq!(get("Camera"), Some("Apple iPhone 15"));
            assert_eq!(get("Lens"), Some("Wide Camera"));
            assert_eq!(get("Taken"), Some("2024-05-01 14:22"));
            assert_eq!(
                get("Exposure"),
                Some("1/250 s · f/2.8 · ISO 400 · 4.2 mm (26 mm equiv.)")
            );
            assert_eq!(get("Location"), Some("48.85840, -2.29450"));
        }
    }

    #[test]
    fn accepts_the_jpeg_prefix_and_survives_garbage() {
        let mut prefixed = b"Exif\0\0".to_vec();
        prefixed.extend(sample(true));
        assert!(!rows(&prefixed).is_empty());
        assert!(rows(b"").is_empty());
        assert!(rows(b"II*\0\xff\xff\xff\xff").is_empty());
        // Truncating anywhere must never panic.
        let full = sample(false);
        for n in 0..full.len() {
            let _ = rows(&full[..n]);
        }
    }

    #[test]
    fn drops_the_brand_when_the_model_repeats_it() {
        let mut b = Builder::new(true);
        let ifd0 = vec![
            Builder::entry(0x010F, Builder::ascii("Canon")),
            Builder::entry(0x0110, Builder::ascii("Canon EOS R5")),
        ];
        let at = b.ifd(&ifd0);
        let first = b.u32(at);
        b.data[4..8].copy_from_slice(&first);
        assert_eq!(
            rows(&b.data),
            [("Camera".to_string(), "Canon EOS R5".to_string())]
        );
    }
}
