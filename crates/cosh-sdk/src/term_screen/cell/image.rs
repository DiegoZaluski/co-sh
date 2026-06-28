//! Images.
//! This module has some helpers for modeling terminal cells that are filled
//! with image data.
//! We're targeting the iTerm image protocol initially, with sixel as an obvious
//! follow up.
//! Kitty has an extensive and complex graphics protocol
//! whose docs are here:
//! <https://github.com/kovidgoyal/kitty/blob/master/docs/graphics-protocol.rst>
//! Both iTerm2 and Sixel appear to have semantics that allow replacing the
//! contents of a single chararcter cell with image data, whereas the kitty
//! protocol appears to track the images out of band as attachments with
//! z-order.

use ordered_float::NotNan;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
// REMOVED: use wezterm_blob_leases::{BlobLease, BlobManager};

fn deserialize_notnan<'de, D>(deserializer: D) -> Result<NotNan<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f32::deserialize(deserializer)?;
    NotNan::new(value).map_err(|e| serde::de::Error::custom(format!("{e:?}")))
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn serialize_notnan<S>(value: &NotNan<f32>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    value.into_inner().serialize(serializer)
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureCoordinate {
    #[serde(
        deserialize_with = "deserialize_notnan",
        serialize_with = "serialize_notnan"
    )]
    pub x: NotNan<f32>,
    #[serde(
        deserialize_with = "deserialize_notnan",
        serialize_with = "serialize_notnan"
    )]
    pub y: NotNan<f32>,
}

impl TextureCoordinate {
    #[must_use]
    pub fn new(x: NotNan<f32>, y: NotNan<f32>) -> Self {
        Self { x, y }
    }

    #[must_use]
    /// # Panics
    /// If either coordinate is NaN.
    pub fn new_f32(x: f32, y: f32) -> Self {
        let x = NotNan::new(x).unwrap();
        let y = NotNan::new(y).unwrap();
        Self::new(x, y)
    }
}

/// Tracks data for displaying an image in the place of the normal cell
/// character data.  Since an Image can span multiple cells, we need to logically
/// carve up the image and track each slice of it.  Each cell needs to know
/// its "texture coordinates" within that image so that we can render the
/// right slice.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ImageCell {
    /// Texture coordinate for the top left of this cell.
    /// (0,0) is the top left of the `ImageData`. (1, 1) is
    /// the bottom right.
    top_left: TextureCoordinate,
    /// Texture coordinates for the bottom right of this cell.
    bottom_right: TextureCoordinate,
    /// References the underlying image data
    data: Arc<ImageData>,
    z_index: i32,
    /// When rendering in the cell, use this offset from the top left
    /// of the cell
    padding_left: u16,
    padding_top: u16,
    padding_right: u16,
    padding_bottom: u16,

    image_id: Option<u32>,
    placement_id: Option<u32>,
}

impl ImageCell {
    pub fn new(
        top_left: TextureCoordinate,
        bottom_right: TextureCoordinate,
        data: Arc<ImageData>,
    ) -> Self {
        Self::with_z_index(top_left, bottom_right, data, 0, 0, 0, 0, 0, None, None)
    }

    pub fn compute_shape_hash<H: Hasher>(&self, hasher: &mut H) {
        self.top_left.hash(hasher);
        self.bottom_right.hash(hasher);
        self.data.hash.hash(hasher);
        self.z_index.hash(hasher);
        self.padding_left.hash(hasher);
        self.padding_top.hash(hasher);
        self.padding_right.hash(hasher);
        self.padding_bottom.hash(hasher);
        self.image_id.hash(hasher);
        self.placement_id.hash(hasher);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_z_index(
        top_left: TextureCoordinate,
        bottom_right: TextureCoordinate,
        data: Arc<ImageData>,
        z_index: i32,
        padding_left: u16,
        padding_top: u16,
        padding_right: u16,
        padding_bottom: u16,
        image_id: Option<u32>,
        placement_id: Option<u32>,
    ) -> Self {
        Self {
            top_left,
            bottom_right,
            data,
            z_index,
            padding_left,
            padding_top,
            padding_right,
            padding_bottom,
            image_id,
            placement_id,
        }
    }

    #[must_use]
    pub fn matches_placement(&self, image_id: u32, placement_id: Option<u32>) -> bool {
        self.image_id == Some(image_id) && self.placement_id == placement_id
    }

    #[must_use]
    pub fn has_placement_id(&self) -> bool {
        self.placement_id.is_some()
    }

    #[must_use]
    pub fn image_id(&self) -> Option<u32> {
        self.image_id
    }

    #[must_use]
    pub fn placement_id(&self) -> Option<u32> {
        self.placement_id
    }

    #[must_use]
    pub fn top_left(&self) -> TextureCoordinate {
        self.top_left
    }

    #[must_use]
    pub fn bottom_right(&self) -> TextureCoordinate {
        self.bottom_right
    }

    #[must_use]
    pub fn image_data(&self) -> &Arc<ImageData> {
        &self.data
    }

    /// negative `z_index` is rendered beneath the text layer.
    /// >= 0 is rendered above the text.
    /// > negative z_index < INT32_MIN/2 will be drawn under cells
    /// > with non-default background colors
    #[must_use]
    pub fn z_index(&self) -> i32 {
        self.z_index
    }

    /// Returns padding (left, top, right, bottom)
    #[must_use]
    pub fn padding(&self) -> (u16, u16, u16, u16) {
        (
            self.padding_left,
            self.padding_top,
            self.padding_right,
            self.padding_bottom,
        )
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum ImageDataType {
    /// Data is in the native image file format
    /// (best for file formats that have animated content)
    EncodedFile(Vec<u8>),
    /// Data is RGBA u8 data
    Rgba8 {
        data: Vec<u8>,
        width: u32,
        height: u32,
        hash: [u8; 32],
    },
    /// Data is an animated sequence
    AnimRgba8 {
        width: u32,
        height: u32,
        durations: Vec<Duration>,
        frames: Vec<Vec<u8>>,
        hashes: Vec<[u8; 32]>,
    },
}

impl std::fmt::Debug for ImageDataType {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::EncodedFile(data) => fmt
                .debug_struct("EncodedFile")
                .field("data_of_len", &data.len())
                .finish(),
            Self::Rgba8 {
                data,
                width,
                height,
                hash,
            } => fmt
                .debug_struct("Rgba8")
                .field("data_of_len", &data.len())
                .field("width", &width)
                .field("height", &height)
                .field("hash", &hash)
                .finish(),
            Self::AnimRgba8 {
                frames,
                width,
                height,
                durations,
                hashes,
            } => fmt
                .debug_struct("AnimRgba8")
                .field("frames_of_len", &frames.len())
                .field("width", &width)
                .field("height", &height)
                .field("durations", durations)
                .field("hashes", hashes)
                .finish(),
        }
    }
}

impl ImageDataType {
    #[must_use]
    /// # Panics
    /// If `data.len()` does not match `width * height * 4`.
    pub fn new_single_frame(width: u32, height: u32, data: Vec<u8>) -> Self {
        let hash = Self::hash_bytes(&data);
        #[allow(clippy::cast_possible_truncation)]
        let len = data.len() as u32;
        assert_eq!(
            width * height * 4,
            len,
            "invalid dimensions {}x{} for pixel data of length {}",
            width,
            height,
            data.len()
        );
        Self::Rgba8 {
            width,
            height,
            data,
            hash,
        }
    }

    /// Black pixels
    #[must_use]
    pub fn placeholder() -> Self {
        let mut data = vec![];
        let size = 8;
        for _ in 0..size * size {
            data.extend_from_slice(&[0, 0, 0, 0xff]);
        }
        ImageDataType::new_single_frame(size, size, data)
    }

    #[must_use]
    pub fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(bytes);
        hasher.finalize().into()
    }

    /// Swap out image data to disk-backed storage.
    /// This is a no-op stub since we removed `wezterm_blob_leases`.
    ///
    /// # Errors
    /// This implementation never returns an error.
    pub fn swap_out(self) -> Result<Self, crate::term_screen::escape_parser::error::InternalError> {
        Ok(self)
    }

    #[must_use]
    pub fn compute_hash(&self) -> [u8; 32] {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        match self {
            ImageDataType::EncodedFile(data) | ImageDataType::Rgba8 { data, .. } => {
                hasher.update(data);
            }
            ImageDataType::AnimRgba8 {
                frames, durations, ..
            } => {
                for data in frames {
                    hasher.update(data);
                }
                for d in durations {
                    let d = d.as_secs_f32();
                    let b = d.to_ne_bytes();
                    hasher.update(b);
                }
            }
        }
        hasher.finalize().into()
    }

    /// Divides the animation frame durations by the provided
    /// `speed_factor`, so a factor of 2 will halve the duration.
    /// # Panics
    /// if the `speed_factor` is negative, non-finite or the result
    /// overflows the allow Duration range.
    pub fn adjust_speed(&mut self, speed_factor: f32) {
        if let Self::AnimRgba8 { durations, .. } = self {
            for d in durations {
                *d = d.mul_f32(1. / speed_factor);
            }
        }
    }

    /// # Errors
    /// Returns an error if the image format is unrecognized or the data is malformed.
    pub fn dimensions(&self) -> Result<(u32, u32), ImageCellError> {
        fn dimensions_for_data(data: &[u8]) -> image::ImageResult<(u32, u32)> {
            let reader =
                image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format()?;
            let (width, height) = reader.into_dimensions()?;

            Ok((width, height))
        }

        match self {
            ImageDataType::EncodedFile(data) => Ok(dimensions_for_data(data)?),
            ImageDataType::AnimRgba8 { width, height, .. }
            | ImageDataType::Rgba8 { width, height, .. } => Ok((*width, *height)),
        }
    }

    /// Decode an encoded file into either an `Rgba8` or `AnimRgba8` variant
    /// if we recognize the file format, otherwise the `EncodedFile` data
    /// is preserved as is.
    #[must_use]
    pub fn decode(self) -> Self {
        use image::{AnimationDecoder, ImageFormat};

        match self {
            Self::EncodedFile(data) => {
                let format = match image::guess_format(&data) {
                    Ok(format) => format,
                    Err(err) => {
                        log::warn!("Unable to decode raw image data: {err:#}");
                        return Self::EncodedFile(data);
                    }
                };
                let cursor = std::io::Cursor::new(&*data);
                match format {
                    ImageFormat::Gif => image::codecs::gif::GifDecoder::new(cursor)
                        .and_then(|decoder| decoder.into_frames().collect_frames())
                        .map_or_else(
                            |err| {
                                log::error!(
                                    "Unable to parse animated gif: {err:#}, trying as single frame"
                                );
                                Self::decode_single(data)
                            },
                            |frames| {
                                if frames.is_empty() {
                                    log::error!("decoded image has 0 frames, using placeholder");
                                    Self::placeholder()
                                } else {
                                    Self::decode_frames(frames)
                                }
                            },
                        ),
                    ImageFormat::Png => {
                        let Ok(decoder) = image::codecs::png::PngDecoder::new(cursor) else {
                            return Self::EncodedFile(data);
                        };
                        if decoder.is_apng().unwrap_or(false) {
                            match decoder
                                .apng()
                                .and_then(|d| d.into_frames().collect_frames())
                            {
                                Ok(frames) if frames.is_empty() => {
                                    log::error!("decoded image has 0 frames, using placeholder");
                                    Self::placeholder()
                                }
                                Ok(frames) => Self::decode_frames(frames),
                                _ => Self::EncodedFile(data),
                            }
                        } else {
                            Self::decode_single(data)
                        }
                    }
                    ImageFormat::WebP => {
                        let Ok(decoder) = image::codecs::webp::WebPDecoder::new(cursor) else {
                            return Self::EncodedFile(data);
                        };
                        match decoder.into_frames().collect_frames() {
                            Ok(frames) if frames.is_empty() => {
                                log::error!("decoded image has 0 frames, using placeholder");
                                Self::placeholder()
                            }
                            Ok(frames) => Self::decode_frames(frames),
                            _ => Self::EncodedFile(data),
                        }
                    }
                    _ => Self::decode_single(data),
                }
            }
            data => data,
        }
    }

    fn decode_frames(img_frames: Vec<image::Frame>) -> Self {
        let mut width = 0;
        let mut height = 0;
        let mut frames = vec![];
        let mut durations = vec![];
        let mut hashes = vec![];
        for frame in img_frames {
            let duration: Duration = frame.delay().into();
            durations.push(duration);
            let image = image::DynamicImage::ImageRgba8(frame.into_buffer()).to_rgba8();
            let (w, h) = image.dimensions();
            width = w;
            height = h;
            let data = image.into_vec();
            hashes.push(Self::hash_bytes(&data));
            frames.push(data);
        }
        Self::AnimRgba8 {
            width,
            height,
            frames,
            durations,
            hashes,
        }
    }

    fn decode_single(data: Vec<u8>) -> Self {
        match image::load_from_memory(&data) {
            Ok(image) => {
                let image = image.to_rgba8();
                let (width, height) = image.dimensions();
                let data = image.into_vec();
                let hash = Self::hash_bytes(&data);
                Self::Rgba8 {
                    width,
                    height,
                    data,
                    hash,
                }
            }
            _ => Self::EncodedFile(data),
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum ImageCellError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    ImageError(#[from] image::ImageError),
}

#[derive(Serialize, Deserialize)]
pub struct ImageData {
    data: Mutex<ImageDataType>,
    hash: [u8; 32],
}

struct HexSlice<'a>(&'a [u8]);
impl std::fmt::Display for HexSlice<'_> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        for byte in self.0 {
            write!(fmt, "{byte:x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("ImageData")
            .field("data", &self.data)
            .field("hash", &format_args!("{}", HexSlice(&self.hash)))
            .finish()
    }
}

impl Eq for ImageData {}
impl PartialEq for ImageData {
    fn eq(&self, rhs: &Self) -> bool {
        self.hash == rhs.hash
    }
}

impl ImageData {
    /// Create a new `ImageData` struct with the provided raw data.
    #[must_use]
    pub fn with_raw_data(data: Vec<u8>) -> Self {
        let hash = ImageDataType::hash_bytes(&data);
        Self::with_data_and_hash(ImageDataType::EncodedFile(data).decode(), hash)
    }

    fn with_data_and_hash(data: ImageDataType, hash: [u8; 32]) -> Self {
        Self {
            data: Mutex::new(data),
            hash,
        }
    }

    #[must_use]
    pub fn with_data(data: ImageDataType) -> Self {
        let hash = data.compute_hash();
        Self {
            data: Mutex::new(data),
            hash,
        }
    }

    /// Returns the in-memory footprint
    pub fn len(&self) -> usize {
        match &*self.data() {
            ImageDataType::EncodedFile(d) => d.len(),
            ImageDataType::Rgba8 { data, .. } => data.len(),
            ImageDataType::AnimRgba8 { frames, .. } => frames.len() * frames[0].len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// # Panics
    /// If the lock is poisoned.
    pub fn data(&self) -> MutexGuard<'_, ImageDataType> {
        self.data.lock().unwrap()
    }

    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
}
