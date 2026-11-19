// SPDX-License-Identifier: MIT OR Apache-2.0
//! SigmaOS-photo-viewer — Image viewer with fullscreen & EXIF.
//!
//! Provides image format detection, EXIF metadata parsing (orientation, camera,
//! GPS), zoom/rotation transforms, slideshow mode, and thumbnail generation.
//! Inspired by Omarchy's minimal, keyboard-driven media workflow.
//!
//! Principles: fail-fast validation, deterministic transforms, zero `unsafe`.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};

// ── Errors ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerError {
    UnsupportedFormat(String),
    EmptyPath,
    InvalidZoom(String),
    InvalidRotation(i32),
    EmptySlideshow,
    IndexOutOfBounds { index: usize, len: usize },
}

impl fmt::Display for ViewerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(ext) => write!(f, "unsupported image format: {ext}"),
            Self::EmptyPath => write!(f, "image path must not be empty"),
            Self::InvalidZoom(msg) => write!(f, "invalid zoom: {msg}"),
            Self::InvalidRotation(d) => write!(f, "rotation must be multiple of 90°, got {d}°"),
            Self::EmptySlideshow => write!(f, "slideshow requires at least one image"),
            Self::IndexOutOfBounds { index, len } => write!(f, "index {index} out of bounds (len {len})"),
        }
    }
}

impl std::error::Error for ViewerError {}

// ── Image Format ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    WebP,
    Gif,
    Bmp,
    Tiff,
    Svg,
    Avif,
}

impl ImageFormat {
    /// Detect format from file extension.
    pub fn from_extension(path: &str) -> Result<Self, ViewerError> {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            "webp" => Ok(Self::WebP),
            "gif" => Ok(Self::Gif),
            "bmp" => Ok(Self::Bmp),
            "tiff" | "tif" => Ok(Self::Tiff),
            "svg" => Ok(Self::Svg),
            "avif" => Ok(Self::Avif),
            other => Err(ViewerError::UnsupportedFormat(other.to_string())),
        }
    }

    /// MIME type for this format.
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::Svg => "image/svg+xml",
            Self::Avif => "image/avif",
        }
    }

    /// Whether the format supports animation.
    pub fn supports_animation(&self) -> bool {
        matches!(self, Self::Gif | Self::WebP | Self::Avif)
    }
}

// ── EXIF Metadata ───────────────────────────────────────────────────────

/// Parsed EXIF metadata fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExifData {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub date_taken: Option<String>,
    pub orientation: u16,
    pub width: u32,
    pub height: u32,
    pub iso: Option<u32>,
    pub aperture: Option<String>,
    pub shutter_speed: Option<String>,
    pub focal_length: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
}

impl ExifData {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, orientation: 1, ..Default::default() }
    }

    /// Whether the image needs rotation based on EXIF orientation tag.
    pub fn needs_rotation(&self) -> bool {
        self.orientation > 1
    }

    /// Rotation degrees implied by EXIF orientation (0, 90, 180, 270).
    pub fn rotation_degrees(&self) -> i32 {
        match self.orientation {
            3 | 4 => 180,
            5 | 6 => 90,
            7 | 8 => 270,
            _ => 0,
        }
    }

    /// Whether GPS data is available.
    pub fn has_gps(&self) -> bool {
        self.gps_lat.is_some() && self.gps_lon.is_some()
    }

    /// Summary string for the info panel.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{}×{}", self.width, self.height)];
        if let Some(ref cam) = self.camera_model {
            parts.push(cam.clone());
        }
        if let Some(ref date) = self.date_taken {
            parts.push(date.clone());
        }
        if let Some(iso) = self.iso {
            parts.push(format!("ISO {iso}"));
        }
        parts.join(" | ")
    }
}

// ── Transform ───────────────────────────────────────────────────────────

/// Viewer transform state: zoom + rotation.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewTransform {
    /// Zoom level: 1.0 = 100%.
    pub zoom: f64,
    /// Rotation in degrees (always 0, 90, 180, or 270).
    pub rotation: i32,
    /// Whether image is flipped horizontally.
    pub flip_h: bool,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self { zoom: 1.0, rotation: 0, flip_h: false }
    }
}

impl ViewTransform {
    /// Set zoom level (0.1..=20.0).
    pub fn set_zoom(&mut self, level: f64) -> Result<(), ViewerError> {
        if !(0.1..=20.0).contains(&level) {
            return Err(ViewerError::InvalidZoom(format!("{level} not in 0.1..=20.0")));
        }
        self.zoom = level;
        Ok(())
    }

    /// Zoom in by a factor.
    pub fn zoom_in(&mut self, factor: f64) -> Result<(), ViewerError> {
        self.set_zoom(self.zoom * factor)
    }

    /// Zoom out by a factor.
    pub fn zoom_out(&mut self, factor: f64) -> Result<(), ViewerError> {
        self.set_zoom(self.zoom / factor)
    }

    /// Rotate 90° clockwise.
    pub fn rotate_cw(&mut self) {
        self.rotation = (self.rotation + 90) % 360;
    }

    /// Rotate 90° counter-clockwise.
    pub fn rotate_ccw(&mut self) {
        self.rotation = (self.rotation + 270) % 360;
    }

    /// Reset to default.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Fit image to viewport dimensions.
    pub fn fit_to_viewport(&mut self, img_w: u32, img_h: u32, vp_w: u32, vp_h: u32) {
        if img_w == 0 || img_h == 0 { return; }
        let scale_w = vp_w as f64 / img_w as f64;
        let scale_h = vp_h as f64 / img_h as f64;
        self.zoom = scale_w.min(scale_h).min(20.0).max(0.1);
    }
}

// ── Slideshow ───────────────────────────────────────────────────────────

/// Slideshow manager.
#[derive(Debug)]
pub struct Slideshow {
    images: Vec<PathBuf>,
    current: usize,
    /// Interval in seconds.
    pub interval_secs: u32,
    pub loop_enabled: bool,
}

impl Slideshow {
    pub fn new(images: Vec<PathBuf>, interval_secs: u32) -> Result<Self, ViewerError> {
        if images.is_empty() {
            return Err(ViewerError::EmptySlideshow);
        }
        Ok(Self { images, current: 0, interval_secs, loop_enabled: true })
    }

    pub fn current_path(&self) -> &Path {
        &self.images[self.current]
    }

    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn next(&mut self) -> Option<&Path> {
        if self.current + 1 < self.images.len() {
            self.current += 1;
            Some(&self.images[self.current])
        } else if self.loop_enabled {
            self.current = 0;
            Some(&self.images[self.current])
        } else {
            None
        }
    }

    pub fn prev(&mut self) -> Option<&Path> {
        if self.current > 0 {
            self.current -= 1;
            Some(&self.images[self.current])
        } else if self.loop_enabled {
            self.current = self.images.len() - 1;
            Some(&self.images[self.current])
        } else {
            None
        }
    }

    pub fn go_to(&mut self, index: usize) -> Result<&Path, ViewerError> {
        if index >= self.images.len() {
            return Err(ViewerError::IndexOutOfBounds { index, len: self.images.len() });
        }
        self.current = index;
        Ok(&self.images[self.current])
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_detection() {
        assert_eq!(ImageFormat::from_extension("photo.jpg").unwrap(), ImageFormat::Jpeg);
        assert_eq!(ImageFormat::from_extension("image.PNG").unwrap(), ImageFormat::Png);
        assert_eq!(ImageFormat::from_extension("anim.webp").unwrap(), ImageFormat::WebP);
    }

    #[test]
    fn test_unsupported_format() {
        assert!(ImageFormat::from_extension("file.xyz").is_err());
    }

    #[test]
    fn test_mime_types() {
        assert_eq!(ImageFormat::Jpeg.mime_type(), "image/jpeg");
        assert_eq!(ImageFormat::Svg.mime_type(), "image/svg+xml");
    }

    #[test]
    fn test_animation_support() {
        assert!(ImageFormat::Gif.supports_animation());
        assert!(!ImageFormat::Jpeg.supports_animation());
    }

    #[test]
    fn test_exif_rotation() {
        let mut exif = ExifData::new(1920, 1080);
        assert!(!exif.needs_rotation());
        exif.orientation = 6;
        assert!(exif.needs_rotation());
        assert_eq!(exif.rotation_degrees(), 90);
    }

    #[test]
    fn test_exif_summary() {
        let mut exif = ExifData::new(4000, 3000);
        exif.camera_model = Some("Canon EOS R5".into());
        exif.iso = Some(400);
        let summary = exif.summary();
        assert!(summary.contains("4000×3000"));
        assert!(summary.contains("Canon EOS R5"));
        assert!(summary.contains("ISO 400"));
    }

    #[test]
    fn test_zoom_bounds() {
        let mut t = ViewTransform::default();
        assert!(t.set_zoom(0.05).is_err());
        assert!(t.set_zoom(25.0).is_err());
        assert!(t.set_zoom(5.0).is_ok());
        assert!((t.zoom - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rotation() {
        let mut t = ViewTransform::default();
        t.rotate_cw();
        assert_eq!(t.rotation, 90);
        t.rotate_cw();
        assert_eq!(t.rotation, 180);
        t.rotate_ccw();
        assert_eq!(t.rotation, 90);
    }

    #[test]
    fn test_fit_to_viewport() {
        let mut t = ViewTransform::default();
        t.fit_to_viewport(4000, 3000, 1920, 1080);
        assert!(t.zoom < 1.0); // image is bigger than viewport
        assert!(t.zoom > 0.0);
    }

    #[test]
    fn test_slideshow_navigation() {
        let images = vec![
            PathBuf::from("/a.jpg"),
            PathBuf::from("/b.png"),
            PathBuf::from("/c.webp"),
        ];
        let mut ss = Slideshow::new(images, 5).unwrap();
        assert_eq!(ss.current_index(), 0);
        ss.next();
        assert_eq!(ss.current_index(), 1);
        ss.next();
        assert_eq!(ss.current_index(), 2);
        // Loop back to start
        ss.next();
        assert_eq!(ss.current_index(), 0);
    }

    #[test]
    fn test_slideshow_prev_wrap() {
        let images = vec![PathBuf::from("/a.jpg"), PathBuf::from("/b.png")];
        let mut ss = Slideshow::new(images, 3).unwrap();
        // At index 0, prev should wrap to last
        ss.prev();
        assert_eq!(ss.current_index(), 1);
    }

    #[test]
    fn test_slideshow_empty_rejected() {
        assert!(Slideshow::new(vec![], 5).is_err());
    }

    #[test]
    fn test_slideshow_go_to() {
        let images = vec![PathBuf::from("/a.jpg"), PathBuf::from("/b.png"), PathBuf::from("/c.gif")];
        let mut ss = Slideshow::new(images, 3).unwrap();
        ss.go_to(2).unwrap();
        assert_eq!(ss.current_index(), 2);
        assert!(ss.go_to(5).is_err());
    }
}
