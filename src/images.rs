//! Download and cache GOG cover art for egui textures.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};

#[derive(Debug, Clone, Copy)]
pub enum CoverSize {
    /// Library list thumbnails.
    Thumb,
    /// Game overview cover.
    Large,
}

/// Normalize a backdrop URL. Does not append a cover-size suffix.
pub fn direct_image_url(raw: &str) -> Option<String> {
    let image = raw.trim();
    if image.is_empty() {
        return None;
    }
    if let Some(rest) = image.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    if image.starts_with("http://") || image.starts_with("https://") {
        return Some(image.to_string());
    }
    Some(format!("https://images.gog-statics.com/{image}"))
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
    if base.contains(".jpg")
        || base.contains(".jpeg")
        || base.contains(".png")
        || base.contains(".webp")
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
    if base.contains(".jpg")
        || base.contains(".jpeg")
        || base.contains(".png")
        || base.contains(".webp")
    {
        None
    } else {
        Some(base)
    }
}

pub struct ImageCache {
    textures: HashMap<String, TextureHandle>,
    /// CPU copy so the backdrop blur can sample a cover without reading the GPU texture.
    pixels: HashMap<String, Arc<ColorImage>>,
    pending: HashSet<String>,
    failed: HashSet<String>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            pixels: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
        }
    }
}

impl ImageCache {
    pub fn texture(&self, url: &str) -> Option<&TextureHandle> {
        self.textures.get(url)
    }

    pub fn pixels(&self, url: &str) -> Option<&Arc<ColorImage>> {
        self.pixels.get(url)
    }

    /// Texture for the first candidate that has already decoded.
    pub fn texture_any(&self, urls: &[String]) -> Option<&TextureHandle> {
        urls.iter().find_map(|url| self.textures.get(url))
    }

    /// Next cover URL to download.
    ///
    /// One candidate is in flight at a time. A later URL is used only after
    /// the earlier one fails, so a library of hundreds of games does not
    /// fetch every fallback size up front.
    pub fn next_to_fetch<'a>(&self, urls: &'a [String]) -> Option<&'a str> {
        if urls.iter().any(|url| self.textures.contains_key(url)) {
            return None;
        }
        if urls.iter().any(|url| self.pending.contains(url)) {
            return None;
        }
        urls.iter()
            .find(|url| !self.failed.contains(url.as_str()))
            .map(String::as_str)
    }

    pub fn any_pending(&self, urls: &[String]) -> bool {
        urls.iter().any(|url| self.pending.contains(url))
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

    pub fn has_failed(&self, url: &str) -> bool {
        self.failed.contains(url)
    }

    /// Upload a cover that was already decoded off the UI thread.
    pub fn insert_decoded(&mut self, ctx: &Context, url: String, image: ColorImage) {
        self.pending.remove(&url);
        let tex = ctx.load_texture(url.clone(), image.clone(), TextureOptions::LINEAR);
        self.textures.insert(url.clone(), tex);
        self.pixels.insert(url, Arc::new(image));
    }
}

pub fn decode_cover(bytes: &[u8]) -> Result<ColorImage, String> {
    let dyn_img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let rgba = dyn_img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_image_url_normalizes_protocol_relative() {
        assert_eq!(
            direct_image_url("//images.gog-statics.com/a.jpg").as_deref(),
            Some("https://images.gog-statics.com/a.jpg")
        );
        assert_eq!(direct_image_url("  ").as_deref(), None);
    }

    #[test]
    fn fetches_one_cover_candidate_at_a_time() {
        let mut cache = ImageCache::default();
        let urls = vec!["a".to_string(), "b".to_string()];
        assert_eq!(cache.next_to_fetch(&urls), Some("a"));
        assert!(cache.request("a".into()));
        assert_eq!(cache.next_to_fetch(&urls), None);
        cache.mark_failed("a");
        assert_eq!(cache.next_to_fetch(&urls), Some("b"));
        assert!(cache.request("b".into()));
        cache.mark_failed("b");
        assert_eq!(cache.next_to_fetch(&urls), None);
    }
}
