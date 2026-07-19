//! Download and cache GOG cover art for egui textures.

use std::collections::{HashMap, HashSet};

use egui::{ColorImage, Context, TextureHandle, TextureOptions};

#[derive(Debug, Clone, Copy)]
pub enum CoverSize {
    /// Library list thumbnails.
    Thumb,
    /// Game overview cover.
    Large,
}

/// Build a full HTTPS cover URL from a GOG `image` field.
pub fn cover_url(image: &str, size: CoverSize) -> Option<String> {
    let image = image.trim();
    if image.is_empty() {
        return None;
    }

    let base = if let Some(rest) = image.strip_prefix("//") {
        format!("https://{rest}")
    } else if image.starts_with("http://") || image.starts_with("https://") {
        image.to_string()
    } else {
        format!("https://images.gog-statics.com/{image}")
    };

    // Already a concrete asset.
    if base.contains(".jpg") || base.contains(".jpeg") || base.contains(".png") || base.contains(".webp")
    {
        return Some(base);
    }

    // GOG serves sized variants by suffix. `_product_tile_196.jpg` is rejected (400);
    // 256+ tiles and product-card covers work.
    let suffix = match size {
        CoverSize::Thumb => "_product_tile_256.jpg",
        CoverSize::Large => "_product_card_v2_mobile_slider_639.jpg",
    };
    Some(format!("{base}{suffix}"))
}

/// Candidate URLs to try for a library/overview image (first success wins).
pub fn cover_url_candidates(image: &str, size: CoverSize) -> Vec<String> {
    let Some(primary) = cover_url(image, size) else {
        return Vec::new();
    };
    let mut out = vec![primary];
    if let Some(base) = image_base(image) {
        for suffix in [
            "_product_tile_256.jpg",
            "_product_tile_398.jpg",
            "_product_card_v2_mobile_slider_639.jpg",
        ] {
            let url = format!("{base}{suffix}");
            if !out.contains(&url) {
                out.push(url);
            }
        }
    }
    out
}

fn image_base(image: &str) -> Option<String> {
    let image = image.trim();
    if image.is_empty() {
        return None;
    }
    let base = if let Some(rest) = image.strip_prefix("//") {
        format!("https://{rest}")
    } else if image.starts_with("http://") || image.starts_with("https://") {
        image.to_string()
    } else {
        format!("https://images.gog-statics.com/{image}")
    };
    if base.contains(".jpg") || base.contains(".jpeg") || base.contains(".png") || base.contains(".webp")
    {
        None
    } else {
        Some(base)
    }
}

pub struct ImageCache {
    textures: HashMap<String, TextureHandle>,
    pending: HashSet<String>,
    failed: HashSet<String>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
        }
    }
}

impl ImageCache {
    pub fn texture(&self, url: &str) -> Option<&TextureHandle> {
        self.textures.get(url)
    }

    pub fn is_pending(&self, url: &str) -> bool {
        self.pending.contains(url)
    }

    pub fn request(&mut self, url: String) -> bool {
        if self.textures.contains_key(&url)
            || self.pending.contains(&url)
            || self.failed.contains(&url)
        {
            return false;
        }
        self.pending.insert(url);
        true
    }

    pub fn mark_failed(&mut self, url: &str) {
        self.pending.remove(url);
        self.failed.insert(url.to_string());
    }

    pub fn insert_bytes(&mut self, ctx: &Context, url: String, bytes: &[u8]) -> bool {
        self.pending.remove(&url);
        match decode_image(bytes) {
            Ok(color) => {
                let tex = ctx.load_texture(url.clone(), color, TextureOptions::LINEAR);
                self.textures.insert(url, tex);
                true
            }
            Err(_) => {
                self.failed.insert(url);
                false
            }
        }
    }
}

fn decode_image(bytes: &[u8]) -> Result<ColorImage, String> {
    let dyn_img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let rgba = dyn_img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}
