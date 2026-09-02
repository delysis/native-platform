use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Cursor, Read, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

use base64::Engine as _;
use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder as _, ImageDecoder as _, ImageFormat, ImageReader, Limits};
use same_file::Handle as FileIdentityHandle;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};

const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_IMAGE_WIDTH: u32 = 8_192;
const MAX_IMAGE_HEIGHT: u32 = 8_192;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_DECODED_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ANIMATION_FRAMES: u32 = 120;
const MAX_PROJECT_IMAGE_ASSETS: u64 = 1_024;
const MAX_PROJECT_IMAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ASSET_DIRECTORY_ENTRIES: u64 = 4_096;

#[derive(Clone, Copy, Debug)]
struct AttachmentLimits {
    image_bytes: usize,
    width: u32,
    height: u32,
    pixels: u64,
    decoded_bytes: u64,
    animation_frames: u32,
    project_assets: u64,
    project_bytes: u64,
    directory_entries: u64,
}

const ATTACHMENT_LIMITS: AttachmentLimits = AttachmentLimits {
    image_bytes: MAX_IMAGE_BYTES,
    width: MAX_IMAGE_WIDTH,
    height: MAX_IMAGE_HEIGHT,
    pixels: MAX_IMAGE_PIXELS,
    decoded_bytes: MAX_DECODED_IMAGE_BYTES,
    animation_frames: MAX_ANIMATION_FRAMES,
    project_assets: MAX_PROJECT_IMAGE_ASSETS,
    project_bytes: MAX_PROJECT_IMAGE_BYTES,
    directory_entries: MAX_ASSET_DIRECTORY_ENTRIES,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct StoredImageAsset {
    pub(crate) relative_path: String,
    pub(crate) markdown_path: String,
    pub(crate) media_type: String,
    pub(crate) byte_count: usize,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoadedImageAsset {
    pub(crate) bytes: Vec<u8>,
    pub(crate) media_type: &'static str,
}

#[derive(Debug, Error)]
pub(crate) enum AttachmentStoreError {
    #[error("the image payload is empty")]
    Empty,
    #[error("the image is larger than Loom's 16 MB attachment limit")]
    TooLarge,
    #[error("the attachment payload is not valid base64")]
    InvalidBase64,
    #[error("the attachment is not a supported PNG, JPEG, GIF, or WebP image")]
    UnsupportedImage,
    #[error("the declared image type does not match its bytes")]
    MediaTypeMismatch,
    #[error("the image is malformed or truncated")]
    MalformedImage,
    #[error(
        "the image exceeds Loom's {max_width} by {max_height} or {max_pixels}-pixel dimension limit"
    )]
    ImageDimensionsExceeded {
        max_width: u32,
        max_height: u32,
        max_pixels: u64,
    },
    #[error(
        "the image exceeds Loom's {max_decoded_bytes}-byte or {max_frames}-frame decode budget"
    )]
    ImageDecodeLimit {
        max_decoded_bytes: u64,
        max_frames: u32,
    },
    #[error("the project image quota is full ({max_assets} images or {max_bytes} aggregate bytes)")]
    ProjectImageQuotaExceeded { max_assets: u64, max_bytes: u64 },
    #[error("the asset directory exceeds Loom's {max_entries}-entry inspection limit")]
    AssetDirectoryEntryLimit { max_entries: u64 },
    #[error("the project asset directory is not a safe ordinary directory")]
    UnsafeAssetDirectory,
    #[error("the content-addressed asset path is not a safe ordinary file")]
    UnsafeAssetPath,
    #[error("the content-addressed asset already exists with different bytes")]
    DigestCollision,
    #[error("the image asset directory could not be durably committed: {0}")]
    DirectoryDurability(std::io::Error),
    #[error("the image attachment could not be stored: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImageKind {
    media_type: &'static str,
    extension: &'static str,
    format: ImageFormat,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AssetInventory {
    count: u64,
    bytes: u64,
}

pub(crate) fn store_image_asset(
    project_root: &Path,
    declared_media_type: &str,
    encoded: &str,
) -> Result<StoredImageAsset, AttachmentStoreError> {
    store_image_asset_with_limits(
        project_root,
        declared_media_type,
        encoded,
        ATTACHMENT_LIMITS,
    )
}

fn store_image_asset_with_limits(
    project_root: &Path,
    declared_media_type: &str,
    encoded: &str,
    limits: AttachmentLimits,
) -> Result<StoredImageAsset, AttachmentStoreError> {
    if encoded.is_empty() {
        return Err(AttachmentStoreError::Empty);
    }
    let max_base64_bytes = limits.image_bytes.div_ceil(3).saturating_mul(4);
    if encoded.len() > max_base64_bytes {
        return Err(AttachmentStoreError::TooLarge);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| AttachmentStoreError::InvalidBase64)?;
    if bytes.is_empty() {
        return Err(AttachmentStoreError::Empty);
    }
    if bytes.len() > limits.image_bytes {
        return Err(AttachmentStoreError::TooLarge);
    }

    let kind = detected_image_kind(&bytes).ok_or(AttachmentStoreError::UnsupportedImage)?;
    if declared_media_type.to_ascii_lowercase() != kind.media_type {
        return Err(AttachmentStoreError::MediaTypeMismatch);
    }
    validate_image(&bytes, kind, limits)?;

    let digest = format!("{:x}", Sha256::digest(&bytes));
    let file_name = format!("{digest}.{}", kind.extension);
    let assets = verified_asset_directory(project_root)?;
    let destination = assets.join(&file_name);

    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(AttachmentStoreError::UnsafeAssetPath);
        }
        Ok(_) => {
            verify_existing(&destination, &bytes, limits.image_bytes)?;
            sync_asset_directory(&assets).map_err(AttachmentStoreError::DirectoryDurability)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            enforce_project_quota(&assets, bytes.len(), limits)?;
            install_content_addressed(&assets, &destination, &bytes, limits.image_bytes)?;
        }
        Err(error) => return Err(error.into()),
    }

    Ok(StoredImageAsset {
        relative_path: format!("assets/{file_name}"),
        markdown_path: format!("../assets/{file_name}"),
        media_type: kind.media_type.to_owned(),
        byte_count: bytes.len(),
        sha256: digest,
    })
}

pub(crate) fn read_image_asset(
    project_root: &Path,
    file_name: &str,
) -> Result<LoadedImageAsset, AttachmentStoreError> {
    let (expected_digest, expected_kind) =
        parse_asset_file_name(file_name).ok_or(AttachmentStoreError::UnsafeAssetPath)?;
    let assets = verified_asset_directory(project_root)?;
    let path = assets.join(file_name);
    let mut file = BoundedAssetFile::open(&path, ATTACHMENT_LIMITS.image_bytes)?;
    let bytes = file.read()?;
    file.ensure_path_binding()?;

    if format!("{:x}", Sha256::digest(&bytes)) != expected_digest {
        return Err(AttachmentStoreError::DigestCollision);
    }
    let actual_kind = detected_image_kind(&bytes).ok_or(AttachmentStoreError::UnsupportedImage)?;
    if actual_kind != expected_kind {
        return Err(AttachmentStoreError::MediaTypeMismatch);
    }
    validate_image(&bytes, actual_kind, ATTACHMENT_LIMITS)?;

    Ok(LoadedImageAsset {
        bytes,
        media_type: actual_kind.media_type,
    })
}

pub(crate) fn verified_asset_directory(
    project_root: &Path,
) -> Result<PathBuf, AttachmentStoreError> {
    require_ordinary_directory(project_root)?;
    let assets = project_root.join("assets");
    require_ordinary_directory(&assets)?;

    let canonical_root = fs::canonicalize(project_root)?;
    let canonical_assets = fs::canonicalize(&assets)?;
    if canonical_assets.parent() != Some(canonical_root.as_path())
        || canonical_assets
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            != Some("assets")
    {
        return Err(AttachmentStoreError::UnsafeAssetDirectory);
    }
    Ok(canonical_assets)
}

fn detected_image_kind(bytes: &[u8]) -> Option<ImageKind> {
    let format = image::guess_format(bytes).ok()?;
    match format {
        ImageFormat::Png => Some(ImageKind {
            media_type: "image/png",
            extension: "png",
            format,
        }),
        ImageFormat::Jpeg => Some(ImageKind {
            media_type: "image/jpeg",
            extension: "jpg",
            format,
        }),
        ImageFormat::Gif => Some(ImageKind {
            media_type: "image/gif",
            extension: "gif",
            format,
        }),
        ImageFormat::WebP => Some(ImageKind {
            media_type: "image/webp",
            extension: "webp",
            format,
        }),
        _ => None,
    }
}

fn parse_asset_file_name(file_name: &str) -> Option<(&str, ImageKind)> {
    let (digest, extension) = file_name.split_once('.')?;
    if digest.len() != 64
        || !digest
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return None;
    }
    let kind = match extension {
        "png" => ImageKind {
            media_type: "image/png",
            extension: "png",
            format: ImageFormat::Png,
        },
        "jpg" => ImageKind {
            media_type: "image/jpeg",
            extension: "jpg",
            format: ImageFormat::Jpeg,
        },
        "gif" => ImageKind {
            media_type: "image/gif",
            extension: "gif",
            format: ImageFormat::Gif,
        },
        "webp" => ImageKind {
            media_type: "image/webp",
            extension: "webp",
            format: ImageFormat::WebP,
        },
        _ => return None,
    };
    Some((digest, kind))
}

pub(crate) fn is_canonical_image_asset_file_name(file_name: &str) -> bool {
    parse_asset_file_name(file_name).is_some()
}

fn validate_image(
    bytes: &[u8],
    kind: ImageKind,
    limits: AttachmentLimits,
) -> Result<(), AttachmentStoreError> {
    let mut reader = ImageReader::with_format(BufReader::new(Cursor::new(bytes)), kind.format);
    reader.limits(image_decode_limits(limits));
    let (width, height) = reader.into_dimensions().map_err(|error| {
        if matches!(error, image::ImageError::Limits(_)) {
            dimension_error(limits)
        } else {
            AttachmentStoreError::MalformedImage
        }
    })?;
    validate_dimensions(width, height, limits)?;

    match kind.format {
        ImageFormat::Gif => validate_gif(bytes, width, height, limits),
        ImageFormat::WebP => validate_webp(bytes, width, height, limits),
        ImageFormat::Png | ImageFormat::Jpeg => {
            let mut reader =
                ImageReader::with_format(BufReader::new(Cursor::new(bytes)), kind.format);
            reader.limits(image_decode_limits(limits));
            reader
                .decode()
                .map(|_| ())
                .map_err(|error| decode_error(&error, limits))
        }
        _ => Err(AttachmentStoreError::UnsupportedImage),
    }
}

fn validate_gif(
    bytes: &[u8],
    width: u32,
    height: u32,
    limits: AttachmentLimits,
) -> Result<(), AttachmentStoreError> {
    let mut decoder = GifDecoder::new(BufReader::new(Cursor::new(bytes)))
        .map_err(|error| decode_error(&error, limits))?;
    decoder
        .set_limits(image_decode_limits(limits))
        .map_err(|error| decode_error(&error, limits))?;
    validate_animation_frames(decoder.into_frames(), width, height, limits)
}

fn validate_webp(
    bytes: &[u8],
    width: u32,
    height: u32,
    limits: AttachmentLimits,
) -> Result<(), AttachmentStoreError> {
    let mut decoder = WebPDecoder::new(BufReader::new(Cursor::new(bytes)))
        .map_err(|error| decode_error(&error, limits))?;
    decoder
        .set_limits(image_decode_limits(limits))
        .map_err(|error| decode_error(&error, limits))?;
    if !decoder.has_animation() {
        let mut reader =
            ImageReader::with_format(BufReader::new(Cursor::new(bytes)), ImageFormat::WebP);
        reader.limits(image_decode_limits(limits));
        return reader
            .decode()
            .map(|_| ())
            .map_err(|error| decode_error(&error, limits));
    }
    validate_animation_frames(decoder.into_frames(), width, height, limits)
}

fn validate_animation_frames(
    frames: image::Frames<'_>,
    width: u32,
    height: u32,
    limits: AttachmentLimits,
) -> Result<(), AttachmentStoreError> {
    let decoded_frame_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| decode_limit_error(limits))?;
    let byte_budget_frames = limits
        .decoded_bytes
        .checked_div(decoded_frame_bytes)
        .unwrap_or(0);
    let allowed_frames = u64::from(limits.animation_frames).min(byte_budget_frames);
    if allowed_frames == 0 {
        return Err(decode_limit_error(limits));
    }

    let mut frame_count = 0_u64;
    for frame in frames {
        frame_count = frame_count.saturating_add(1);
        if frame_count > allowed_frames {
            return Err(decode_limit_error(limits));
        }
        let frame = frame.map_err(|error| decode_error(&error, limits))?;
        if frame.buffer().width() != width || frame.buffer().height() != height {
            return Err(AttachmentStoreError::MalformedImage);
        }
    }
    if frame_count == 0 {
        return Err(AttachmentStoreError::MalformedImage);
    }
    Ok(())
}

fn image_decode_limits(limits: AttachmentLimits) -> Limits {
    let mut decode_limits = Limits::default();
    decode_limits.max_image_width = Some(limits.width);
    decode_limits.max_image_height = Some(limits.height);
    decode_limits.max_alloc = Some(limits.decoded_bytes);
    decode_limits
}

fn validate_dimensions(
    width: u32,
    height: u32,
    limits: AttachmentLimits,
) -> Result<(), AttachmentStoreError> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > limits.width
        || height > limits.height
        || pixels > limits.pixels
    {
        return Err(dimension_error(limits));
    }
    let decoded_bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| decode_limit_error(limits))?;
    if decoded_bytes > limits.decoded_bytes {
        return Err(decode_limit_error(limits));
    }
    Ok(())
}

fn decode_error(error: &image::ImageError, limits: AttachmentLimits) -> AttachmentStoreError {
    if matches!(error, image::ImageError::Limits(_)) {
        decode_limit_error(limits)
    } else {
        AttachmentStoreError::MalformedImage
    }
}

fn dimension_error(limits: AttachmentLimits) -> AttachmentStoreError {
    AttachmentStoreError::ImageDimensionsExceeded {
        max_width: limits.width,
        max_height: limits.height,
        max_pixels: limits.pixels,
    }
}

fn decode_limit_error(limits: AttachmentLimits) -> AttachmentStoreError {
    AttachmentStoreError::ImageDecodeLimit {
        max_decoded_bytes: limits.decoded_bytes,
        max_frames: limits.animation_frames,
    }
}

fn require_ordinary_directory(path: &Path) -> Result<(), AttachmentStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(AttachmentStoreError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AttachmentStoreError::UnsafeAssetDirectory);
    }
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(AttachmentStoreError::UnsafeAssetDirectory);
    }
    Ok(())
}

fn enforce_project_quota(
    assets: &Path,
    candidate_bytes: usize,
    limits: AttachmentLimits,
) -> Result<(), AttachmentStoreError> {
    let inventory = asset_inventory(assets, limits)?;
    let next_count = inventory.count.saturating_add(1);
    let next_bytes = inventory
        .bytes
        .saturating_add(u64::try_from(candidate_bytes).unwrap_or(u64::MAX));
    if next_count > limits.project_assets || next_bytes > limits.project_bytes {
        return Err(AttachmentStoreError::ProjectImageQuotaExceeded {
            max_assets: limits.project_assets,
            max_bytes: limits.project_bytes,
        });
    }
    Ok(())
}

fn asset_inventory(
    assets: &Path,
    limits: AttachmentLimits,
) -> Result<AssetInventory, AttachmentStoreError> {
    let mut inventory = AssetInventory::default();
    let mut inspected = 0_u64;
    for entry in fs::read_dir(assets)? {
        let entry = entry?;
        inspected = inspected.saturating_add(1);
        if inspected > limits.directory_entries {
            return Err(AttachmentStoreError::AssetDirectoryEntryLimit {
                max_entries: limits.directory_entries,
            });
        }
        let file_name = entry.file_name();
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AttachmentStoreError::UnsafeAssetPath);
        }
        #[cfg(windows)]
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AttachmentStoreError::UnsafeAssetPath);
        }
        let managed_image = file_name
            .to_str()
            .is_some_and(is_canonical_image_asset_file_name);
        if managed_image && metadata.len() > u64::try_from(limits.image_bytes).unwrap_or(u64::MAX) {
            return Err(AttachmentStoreError::UnsafeAssetPath);
        }
        inventory.count = inventory.count.saturating_add(1);
        inventory.bytes = inventory.bytes.saturating_add(metadata.len());
        if inventory.count > limits.project_assets || inventory.bytes > limits.project_bytes {
            return Err(AttachmentStoreError::ProjectImageQuotaExceeded {
                max_assets: limits.project_assets,
                max_bytes: limits.project_bytes,
            });
        }
    }
    Ok(inventory)
}

fn install_content_addressed(
    assets: &Path,
    destination: &Path,
    bytes: &[u8],
    max_image_bytes: usize,
) -> Result<(), AttachmentStoreError> {
    install_content_addressed_with_sync(
        assets,
        destination,
        bytes,
        max_image_bytes,
        sync_asset_directory,
    )
}

fn install_content_addressed_with_sync(
    assets: &Path,
    destination: &Path,
    bytes: &[u8],
    max_image_bytes: usize,
    sync_directory: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), AttachmentStoreError> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(AttachmentStoreError::UnsafeAssetPath);
        }
        Ok(_) => {
            verify_existing(destination, bytes, max_image_bytes)?;
            sync_directory(assets).map_err(AttachmentStoreError::DirectoryDurability)?;
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let temporary = create_durable_temporary(assets, bytes)?;
    let linked = match fs::hard_link(&temporary, destination) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
    };

    // The destination is committed once the hard link succeeds. Temporary-file
    // cleanup must never turn that committed, idempotently verifiable result
    // into a false storage failure.
    let _ = fs::remove_file(&temporary);
    if linked {
        sync_directory(assets).map_err(AttachmentStoreError::DirectoryDurability)?;
        Ok(())
    } else {
        verify_existing(destination, bytes, max_image_bytes)?;
        sync_directory(assets).map_err(AttachmentStoreError::DirectoryDurability)
    }
}

fn create_durable_temporary(assets: &Path, bytes: &[u8]) -> Result<PathBuf, AttachmentStoreError> {
    for _ in 0..16 {
        let path = assets.join(format!(
            ".loom-attachment-{}",
            loom_types::ArtifactId::new()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&path);
                    return Err(error.into());
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(AttachmentStoreError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique attachment staging file",
    )))
}

fn verify_existing(
    path: &Path,
    expected: &[u8],
    max_image_bytes: usize,
) -> Result<(), AttachmentStoreError> {
    let mut file = BoundedAssetFile::open(path, max_image_bytes)?;
    let actual = file.read()?;
    file.ensure_path_binding()?;
    if actual == expected {
        Ok(())
    } else {
        Err(AttachmentStoreError::DigestCollision)
    }
}

#[cfg(unix)]
fn sync_asset_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_asset_directory(path: &Path) -> std::io::Result<()> {
    // Rust exposes no portable Windows directory-flush primitive. Opening a
    // directory as a regular file fails after the hard-link commit, so validate
    // the directory boundary instead of manufacturing a false write failure.
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "asset directory is not an ordinary directory",
        ));
    }
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "asset directory is a reparse point",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct BoundedAssetFile {
    file: File,
    identity: FileIdentityHandle,
    path: PathBuf,
    max_bytes: usize,
}

impl BoundedAssetFile {
    fn open(path: &Path, max_bytes: usize) -> Result<Self, AttachmentStoreError> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        #[cfg(windows)]
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);

        let file = match options.open(path) {
            Ok(file) => file,
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(AttachmentStoreError::UnsafeAssetPath);
            }
            Err(error) => return Err(error.into()),
        };
        validate_asset_metadata(&file.metadata()?, max_bytes)?;
        let identity = FileIdentityHandle::from_file(file.try_clone()?)?;
        Ok(Self {
            file,
            identity,
            path: path.to_path_buf(),
            max_bytes,
        })
    }

    fn read(&mut self) -> Result<Vec<u8>, AttachmentStoreError> {
        let metadata = self.file.metadata()?;
        validate_asset_metadata(&metadata, self.max_bytes)?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        Read::by_ref(&mut self.file)
            .take(
                u64::try_from(self.max_bytes)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)?;
        if bytes.len() > self.max_bytes {
            return Err(AttachmentStoreError::TooLarge);
        }
        Ok(bytes)
    }

    fn ensure_path_binding(&self) -> Result<(), AttachmentStoreError> {
        validate_asset_metadata(&fs::symlink_metadata(&self.path)?, self.max_bytes)?;
        let visible = FileIdentityHandle::from_path(&self.path)?;
        validate_asset_metadata(&fs::symlink_metadata(&self.path)?, self.max_bytes)?;
        if visible != self.identity {
            return Err(AttachmentStoreError::UnsafeAssetPath);
        }
        Ok(())
    }
}

fn validate_asset_metadata(
    metadata: &fs::Metadata,
    max_bytes: usize,
) -> Result<(), AttachmentStoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AttachmentStoreError::UnsafeAssetPath);
    }
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(AttachmentStoreError::UnsafeAssetPath);
    }
    if metadata.len() > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
        return Err(AttachmentStoreError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_image(format: ImageFormat, width: u32, height: u32, seed: u8) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([
                seed.wrapping_add(u8::try_from(x).unwrap_or(u8::MAX)),
                seed.wrapping_add(u8::try_from(y).unwrap_or(u8::MAX)),
                seed,
                u8::MAX,
            ])
        });
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, format)
            .expect("encode real image fixture");
        encoded.into_inner()
    }

    fn prepared_project() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temporary project");
        fs::create_dir(root.path().join("assets")).expect("asset directory");
        root
    }

    fn store_bytes(
        root: &Path,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<StoredImageAsset, AttachmentStoreError> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        store_image_asset(root, media_type, &encoded)
    }

    #[test]
    fn stores_reads_and_reuses_structurally_valid_supported_images() {
        let root = prepared_project();
        for (format, media_type, seed) in [
            (ImageFormat::Png, "image/png", 1),
            (ImageFormat::Jpeg, "image/jpeg", 2),
            (ImageFormat::Gif, "image/gif", 3),
            (ImageFormat::WebP, "image/webp", 4),
        ] {
            let bytes = encoded_image(format, 2, 2, seed);
            let first = store_bytes(root.path(), media_type, &bytes)
                .unwrap_or_else(|error| panic!("first {format:?} store: {error:?}"));
            let second = store_bytes(root.path(), media_type, &bytes).expect("idempotent store");
            assert_eq!(first, second);
            assert_eq!(
                fs::read(root.path().join(&first.relative_path)).expect("asset bytes"),
                bytes
            );
            assert_eq!(first.markdown_path, format!("../{}", first.relative_path));
            let file_name = Path::new(&first.relative_path)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .expect("asset file name");
            let loaded = read_image_asset(root.path(), file_name).expect("safe asset read");
            assert_eq!(loaded.bytes, bytes);
            assert_eq!(loaded.media_type, media_type);
        }
    }

    #[test]
    fn rejects_magic_prefixes_without_a_complete_image_structure() {
        let root = prepared_project();
        let fake_png = b"\x89PNG\r\n\x1a\nsmall-test-payload";
        assert!(matches!(
            store_bytes(root.path(), "image/png", fake_png),
            Err(AttachmentStoreError::MalformedImage)
        ));

        let mut truncated = encoded_image(ImageFormat::Jpeg, 2, 2, 4);
        truncated.truncate(truncated.len() / 2);
        assert!(matches!(
            store_bytes(root.path(), "image/jpeg", &truncated),
            Err(AttachmentStoreError::MalformedImage)
        ));
    }

    #[test]
    fn enforces_dimension_and_decompression_limits_before_storage() {
        let root = prepared_project();
        let dimensions = encoded_image(ImageFormat::Png, 3, 1, 1);
        let encoded = base64::engine::general_purpose::STANDARD.encode(dimensions);
        let mut limits = ATTACHMENT_LIMITS;
        limits.width = 2;
        assert!(matches!(
            store_image_asset_with_limits(root.path(), "image/png", &encoded, limits),
            Err(AttachmentStoreError::ImageDimensionsExceeded { .. })
        ));

        let decoded = encoded_image(ImageFormat::Png, 2, 2, 2);
        let encoded = base64::engine::general_purpose::STANDARD.encode(decoded);
        let mut limits = ATTACHMENT_LIMITS;
        limits.decoded_bytes = 15;
        assert!(matches!(
            store_image_asset_with_limits(root.path(), "image/png", &encoded, limits),
            Err(AttachmentStoreError::ImageDecodeLimit { .. })
        ));
    }

    #[test]
    fn aggregate_count_and_byte_quotas_preserve_content_addressed_deduplication() {
        let root = prepared_project();
        let first_bytes = encoded_image(ImageFormat::Png, 2, 2, 1);
        let second_bytes = encoded_image(ImageFormat::Png, 2, 2, 2);
        let first_encoded = base64::engine::general_purpose::STANDARD.encode(&first_bytes);
        let second_encoded = base64::engine::general_purpose::STANDARD.encode(&second_bytes);

        let mut count_limits = ATTACHMENT_LIMITS;
        count_limits.project_assets = 1;
        let first =
            store_image_asset_with_limits(root.path(), "image/png", &first_encoded, count_limits)
                .expect("first quota item");
        let replay =
            store_image_asset_with_limits(root.path(), "image/png", &first_encoded, count_limits)
                .expect("deduplicated replay does not consume quota");
        assert_eq!(first, replay);
        assert!(matches!(
            store_image_asset_with_limits(root.path(), "image/png", &second_encoded, count_limits,),
            Err(AttachmentStoreError::ProjectImageQuotaExceeded { .. })
        ));

        let byte_root = prepared_project();
        let mut byte_limits = ATTACHMENT_LIMITS;
        byte_limits.project_bytes = u64::try_from(first_bytes.len()).expect("fixture size");
        store_image_asset_with_limits(byte_root.path(), "image/png", &first_encoded, byte_limits)
            .expect("first byte quota item");
        assert!(matches!(
            store_image_asset_with_limits(
                byte_root.path(),
                "image/png",
                &second_encoded,
                byte_limits,
            ),
            Err(AttachmentStoreError::ProjectImageQuotaExceeded { .. })
        ));
    }

    #[test]
    fn rejects_declared_type_spoofing_and_unbounded_payloads() {
        let root = prepared_project();
        let png = encoded_image(ImageFormat::Png, 2, 2, 1);
        assert!(matches!(
            store_bytes(root.path(), "image/jpeg", &png),
            Err(AttachmentStoreError::MediaTypeMismatch)
        ));
        let max_base64_bytes = MAX_IMAGE_BYTES.div_ceil(3) * 4;
        assert!(matches!(
            store_image_asset(root.path(), "image/png", &"A".repeat(max_base64_bytes + 1)),
            Err(AttachmentStoreError::TooLarge)
        ));
    }

    #[test]
    fn reads_reject_digest_and_extension_mismatches() {
        let digest_root = prepared_project();
        let original = encoded_image(ImageFormat::Png, 2, 2, 1);
        let replacement = encoded_image(ImageFormat::Png, 2, 2, 2);
        let stored = store_bytes(digest_root.path(), "image/png", &original).expect("store image");
        let file_name = Path::new(&stored.relative_path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("asset file name");
        fs::write(digest_root.path().join(&stored.relative_path), replacement)
            .expect("tamper asset bytes");
        assert!(matches!(
            read_image_asset(digest_root.path(), file_name),
            Err(AttachmentStoreError::DigestCollision)
        ));

        let extension_root = prepared_project();
        let digest = format!("{:x}", Sha256::digest(&original));
        let mismatched_name = format!("{digest}.jpg");
        fs::write(
            extension_root.path().join("assets").join(&mismatched_name),
            original,
        )
        .expect("write extension-mismatched image");
        assert!(matches!(
            read_image_asset(extension_root.path(), &mismatched_name),
            Err(AttachmentStoreError::MediaTypeMismatch)
        ));
    }

    #[test]
    fn aggregate_quota_counts_every_ordinary_project_asset_entry() {
        let root = prepared_project();
        fs::write(
            root.path().join("assets").join("preexisting.bin"),
            b"manual",
        )
        .expect("preexisting ordinary asset");
        let png = encoded_image(ImageFormat::Png, 2, 2, 3);
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let mut limits = ATTACHMENT_LIMITS;
        limits.project_assets = 1;
        assert!(matches!(
            store_image_asset_with_limits(root.path(), "image/png", &encoded, limits),
            Err(AttachmentStoreError::ProjectImageQuotaExceeded { .. })
        ));
    }

    #[test]
    fn post_commit_sync_failure_is_typed_and_idempotently_recoverable() {
        use std::cell::Cell;

        let root = prepared_project();
        let bytes = encoded_image(ImageFormat::Png, 2, 2, 1);
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let destination = root.path().join("assets").join(format!("{digest}.png"));
        let failure = install_content_addressed_with_sync(
            &root.path().join("assets"),
            &destination,
            &bytes,
            MAX_IMAGE_BYTES,
            |_| Err(std::io::Error::other("injected directory sync failure")),
        )
        .expect_err("sync failure is not silently promoted to durable success");
        assert!(matches!(
            failure,
            AttachmentStoreError::DirectoryDurability(_)
        ));
        assert_eq!(
            fs::read(&destination).expect("committed destination"),
            bytes
        );

        let retry_synced = Cell::new(false);
        install_content_addressed_with_sync(
            &root.path().join("assets"),
            &destination,
            &bytes,
            MAX_IMAGE_BYTES,
            |_| {
                retry_synced.set(true);
                Ok(())
            },
        )
        .expect("idempotent retry verifies and syncs the committed destination");
        assert!(retry_synced.get());
    }

    #[test]
    fn platform_directory_durability_boundary_accepts_an_ordinary_directory() {
        let root = prepared_project();
        sync_asset_directory(&root.path().join("assets"))
            .expect("platform-specific directory durability boundary");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_asset_directories_and_files() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temporary project");
        let outside = tempfile::tempdir().expect("outside directory");
        symlink(outside.path(), root.path().join("assets")).expect("asset symlink");
        let png = encoded_image(ImageFormat::Png, 2, 2, 1);
        assert!(matches!(
            store_bytes(root.path(), "image/png", &png),
            Err(AttachmentStoreError::UnsafeAssetDirectory)
        ));
        assert!(matches!(
            verified_asset_directory(root.path()),
            Err(AttachmentStoreError::UnsafeAssetDirectory)
        ));

        let safe_root = prepared_project();
        let stored = store_bytes(safe_root.path(), "image/png", &png).expect("stored image");
        let file_name = Path::new(&stored.relative_path)
            .file_name()
            .expect("file name")
            .to_owned();
        fs::remove_file(safe_root.path().join(&stored.relative_path)).expect("remove asset");
        let external = outside.path().join("image.png");
        fs::write(&external, png).expect("external image");
        symlink(&external, safe_root.path().join("assets").join(&file_name))
            .expect("asset file symlink");
        assert!(matches!(
            read_image_asset(
                safe_root.path(),
                file_name.to_str().expect("utf-8 file name")
            ),
            Err(AttachmentStoreError::UnsafeAssetPath)
        ));
    }
}
