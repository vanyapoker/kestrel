//! In-memory HEIF decoding using the system libheif library.

use std::path::{Path, PathBuf};

use image::{DynamicImage, RgbaImage};
use libheif_rs::{ColorSpace, HeifContext, HeifErrorCode, ImageHandle, LibHeif, RgbChroma};

const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_DECODED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_IMAGES: usize = 256;

enum Contents {
    Stills(Vec<u32>),
    Sequence(usize),
}

/// Reopens the file on each worker, keeping libheif handles on their owning thread.
pub struct Images {
    path: PathBuf,
    contents: Contents,
}

fn context(path: &Path) -> Result<HeifContext<'static>, String> {
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > MAX_FILE_BYTES {
        return Err("Image too large to preview".into());
    }
    let path = path.to_str().ok_or("HEIC path is not valid UTF-8")?;
    let mut context = HeifContext::read_from_file(path).map_err(|e| format!("HEIC read: {e}"))?;
    context.set_max_decoding_threads(0);
    Ok(context)
}

impl Images {
    pub fn open(path: &Path) -> Result<Self, String> {
        let ctx = context(path)?;
        let contents = if ctx.has_sequence() {
            let track = ctx.track(0).ok_or("HEIC sequence has no image track")?;
            if let Ok(size) = track.image_resolution()
                && u64::from(size.width) * u64::from(size.height) * 4 > MAX_DECODED_BYTES
            {
                return Err("Image too large to preview".into());
            }
            let mut count = 0;
            loop {
                if crate::thumbs::is_cancelled() {
                    return Err("HEIC preview cancelled".into());
                }
                match track.decode_next_image(ColorSpace::Rgb(RgbChroma::Rgba), None) {
                    Ok(_) if count < MAX_IMAGES => count += 1,
                    Ok(_) => return Err("HEIC sequence has too many frames".into()),
                    Err(e) if matches!(e.code, HeifErrorCode::EndOfSequence) => break,
                    Err(e) => return Err(format!("HEIC sequence: {e}")),
                }
            }
            if count == 0 {
                return Err("HEIC sequence has no frames".into());
            }
            Contents::Sequence(count)
        } else {
            let mut ids = ctx.image_ids();
            let primary = ctx
                .primary_image_handle()
                .map_err(|e| format!("HEIC read: {e}"))?
                .item_id();
            if ids.len() > MAX_IMAGES {
                return Err("HEIC image contains too many images".into());
            }
            if let Some(i) = ids.iter().position(|id| *id == primary) {
                ids.swap(0, i);
            }
            if ids.is_empty() {
                return Err("HEIC image contains no images".into());
            }
            Contents::Stills(ids)
        };
        Ok(Self {
            path: path.to_path_buf(),
            contents,
        })
    }

    pub fn len(&self) -> usize {
        match &self.contents {
            Contents::Stills(ids) => ids.len(),
            Contents::Sequence(count) => *count,
        }
    }

    /// Decode a primary image's embedded thumbnail without decoding the full image.
    pub fn thumbnail(path: &Path, max: u32) -> Option<eframe::egui::ColorImage> {
        let ctx = context(path).ok()?;
        let primary = ctx.primary_image_handle().ok()?;
        let mut ids = vec![0; primary.number_of_thumbnails().min(MAX_IMAGES)];
        let count = primary.thumbnail_ids(&mut ids);
        ids.truncate(count);
        let thumb = ids
            .into_iter()
            .filter_map(|id| primary.thumbnail(id).ok())
            .max_by_key(|handle| u64::from(handle.width()) * u64::from(handle.height()))?;
        decode_handle_with_profile(&thumb, max, color_profile(&primary))
            .ok()
            .map(|r| r.image)
    }

    pub fn render(&self, index: usize, max: u32) -> Result<crate::thumbs::Rendered, String> {
        if index >= self.len() {
            return Err("Image index out of range".into());
        }
        let ctx = context(&self.path)?;
        match &self.contents {
            Contents::Stills(ids) => {
                let handle = ctx
                    .image_handle(ids[index])
                    .map_err(|e| format!("HEIC read: {e}"))?;
                decode_handle(&handle, max)
            }
            Contents::Sequence(_) => {
                let track = ctx.track(0).ok_or("HEIC sequence has no image track")?;
                let size = track
                    .image_resolution()
                    .ok()
                    .map(|s| (u32::from(s.width), u32::from(s.height)));
                for frame in 0..=index {
                    if crate::thumbs::is_cancelled() {
                        return Err("HEIC preview cancelled".into());
                    }
                    let image = track
                        .decode_next_image(ColorSpace::Rgb(RgbChroma::Rgba), None)
                        .map_err(|e| format!("HEIC sequence: {e}"))?;
                    if frame == index {
                        return render_image(&image, max, size, None, None);
                    }
                }
                Err("HEIC frame unavailable".into())
            }
        }
    }
}

fn decode_handle(handle: &ImageHandle, max: u32) -> Result<crate::thumbs::Rendered, String> {
    decode_handle_with_profile(handle, max, None)
}

fn decode_handle_with_profile(
    handle: &ImageHandle,
    max: u32,
    fallback_profile: Option<Vec<u8>>,
) -> Result<crate::thumbs::Rendered, String> {
    if u64::from(handle.width()) * u64::from(handle.height()) * 4 > MAX_DECODED_BYTES {
        return Err("Image too large to preview".into());
    }
    let image = LibHeif::new()
        .decode(handle, ColorSpace::Rgb(RgbChroma::Rgba), None)
        .map_err(|e| format!("HEIC decode: {e}"))?;
    render_image(
        &image,
        max,
        None,
        read_exif(handle),
        color_profile(handle).or(fallback_profile),
    )
}

fn render_image(
    image: &libheif_rs::Image,
    max: u32,
    crop: Option<(u32, u32)>,
    exif: Option<Vec<u8>>,
    profile: Option<Vec<u8>>,
) -> Result<crate::thumbs::Rendered, String> {
    let plane = image
        .planes()
        .interleaved
        .ok_or("HEIC decoder returned no pixels")?;
    let (w, h) = (plane.width, plane.height);
    if u64::from(w) * u64::from(h) * 4 > MAX_DECODED_BYTES {
        return Err("Image too large to preview".into());
    }
    let row = (w as usize)
        .checked_mul(4)
        .ok_or("Invalid HEIC dimensions")?;
    if plane.stride < row {
        return Err("Invalid HEIC pixel stride".into());
    }
    let mut pixels = Vec::with_capacity(row * h as usize);
    for line in plane.data.chunks(plane.stride).take(h as usize) {
        pixels.extend_from_slice(line.get(..row).ok_or("Invalid HEIC pixels")?);
    }
    let rgba = RgbaImage::from_raw(w, h, pixels).ok_or("Invalid HEIC pixels")?;
    let mut img = DynamicImage::ImageRgba8(rgba);
    if let Some((cw, ch)) = crop.filter(|&(cw, ch)| cw > 0 && ch > 0 && cw <= w && ch <= h) {
        img = img.crop_imm(0, 0, cw, ch);
    }
    let dims = (img.width(), img.height());
    if dims.0 > max || dims.1 > max {
        img = img.thumbnail(max, max);
    }
    let rgba = to_srgb(img.to_rgba8(), profile.as_deref());
    Ok(crate::thumbs::Rendered {
        image: eframe::egui::ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        ),
        dims: Some(dims),
        exif,
    })
}

fn read_exif(handle: &ImageHandle) -> Option<Vec<u8>> {
    let mut ids = [0];
    if handle.metadata_block_ids(&mut ids, b"Exif") == 0
        || handle.metadata_size(ids[0]) > 1024 * 1024
    {
        return None;
    }
    let bytes = handle.metadata(ids[0]).ok()?;
    let start = bytes
        .windows(4)
        .position(|b| b == b"II*\0" || b == b"MM\0*")?;
    Some(bytes[start..].to_vec())
}

fn color_profile(handle: &ImageHandle) -> Option<Vec<u8>> {
    if let Some(profile) = handle.color_profile_raw() {
        return Some(profile.data);
    }
    let profile = handle.color_profile_nclx()?;
    let cicp = moxcms::CicpProfile {
        color_primaries: moxcms::CicpColorPrimaries::try_from(profile.color_primaries() as u8)
            .ok()?,
        transfer_characteristics: moxcms::TransferCharacteristics::try_from(
            profile.transfer_characteristics() as u8,
        )
        .ok()?,
        matrix_coefficients: moxcms::MatrixCoefficients::try_from(0u8).ok()?,
        full_range: true,
    };
    moxcms::ColorProfile::new_from_cicp(cicp).encode().ok()
}

fn to_srgb(image: RgbaImage, profile: Option<&[u8]>) -> RgbaImage {
    let Some(profile) = profile.and_then(|p| moxcms::ColorProfile::new_from_slice(p).ok()) else {
        return image;
    };
    let gray = match profile.color_space {
        moxcms::DataColorSpace::Rgb => false,
        moxcms::DataColorSpace::Gray => true,
        _ => return image,
    };
    let Ok(transform) = profile.create_transform_8bit(
        if gray {
            moxcms::Layout::Gray
        } else {
            moxcms::Layout::Rgba
        },
        &moxcms::ColorProfile::new_srgb(),
        moxcms::Layout::Rgba,
        moxcms::TransformOptions::default(),
    ) else {
        return image;
    };
    let gray_data;
    let input = if gray {
        gray_data = image.pixels().map(|pixel| pixel[0]).collect::<Vec<_>>();
        gray_data.as_slice()
    } else {
        image.as_raw().as_slice()
    };
    let mut output = vec![0; image.as_raw().len()];
    if transform.transform(input, &mut output).is_err() {
        return image;
    }
    for (out, original) in output.as_chunks_mut::<4>().0.iter_mut().zip(image.pixels()) {
        out[3] = original[3];
    }
    RgbaImage::from_raw(image.width(), image.height(), output).unwrap_or(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn decodes_collections_and_sequences() {
        for name in ["heif-stills.heic", "heif-sequence.heic"] {
            let images = Images::open(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(images.len(), 2, "{name}");
            let first = images.render(0, 32).unwrap();
            let second = images.render(1, 32).unwrap();
            assert_eq!(first.dims, Some((64, 48)), "{name}");
            assert_eq!(first.image.size, [32, 24]);
            let a = first.image.pixels[0];
            let b = second.image.pixels[0];
            assert!(a.r() > a.b() && b.b() > b.r(), "{name}: {a:?}, {b:?}");
        }
    }

    #[test]
    fn embedded_thumbnail_is_upright() {
        let path = fixture("heif-embedded.heic");
        let thumbnail = Images::thumbnail(&path, 128).expect("embedded thumbnail");
        let full = Images::open(&path).unwrap().render(0, 128).unwrap().image;
        assert_eq!(thumbnail.size, [96, 128]);
        assert_eq!(thumbnail.size, full.size);
        assert!(thumbnail.pixels[0].r() > thumbnail.pixels[0].b() * 2);
        assert!(Images::thumbnail(&fixture("heif-stills.heic"), 128).is_none());
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(
            Images::open(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("Cargo.toml")
                    .as_path()
            )
            .is_err()
        );
    }

    #[test]
    fn converts_color_profile_and_preserves_alpha() {
        let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let original = RgbaImage::from_pixel(1, 1, image::Rgba([180, 80, 45, 107]));
        let converted = to_srgb(original.clone(), Some(&profile));
        assert_eq!(converted[(0, 0)][3], 107);
        assert_ne!(converted[(0, 0)], original[(0, 0)]);
        assert_eq!(to_srgb(original.clone(), None), original);
    }

    #[test]
    fn optional_real_photo_thumbnail_matches_preview_orientation() {
        let Some(path) = std::env::var_os("KESTREL_TEST_HEIC") else {
            return;
        };
        let path = PathBuf::from(path);
        let started = std::time::Instant::now();
        let thumbnail = Images::thumbnail(&path, 256).expect("embedded thumbnail");
        let thumb_elapsed = started.elapsed();
        let started = std::time::Instant::now();
        let preview = Images::open(&path).unwrap().render(0, 256).unwrap();
        eprintln!(
            "embedded thumbnail: {thumb_elapsed:?}, full preview: {:?}",
            started.elapsed()
        );
        assert_eq!(
            thumbnail.size[0] > thumbnail.size[1],
            preview.image.size[0] > preview.image.size[1]
        );
        assert!(preview.exif.is_some());
    }
}
