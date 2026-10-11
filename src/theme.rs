//! Frosted-glass desktop theme: dark ambient gradient, translucent panels, neutral lift.

use std::sync::Arc;

use egui::style::WidgetVisuals;
use egui::{
    Color32, ColorImage, CornerRadius, FontFamily, FontId, Frame, Id, Margin, Mesh, Painter, Pos2,
    ProgressBar, Rect, RichText, Shadow, Shape, Stroke, Style, TextStyle, TextureOptions, Vec2,
    Visuals,
};

use crate::backdrop;

/// Canonical interactive control height. All standard buttons share this.
pub const BTN_HEIGHT: f32 = 36.0;

/// Fixed-width side-column actions (Add / Remove / Clear / Refresh).
pub const SIDE_BTN: egui::Vec2 = egui::vec2(88.0, BTN_HEIGHT);

/// Primary CTA width (Burn / Reburn); height matches [`BTN_HEIGHT`].
pub const PRIMARY_BTN: egui::Vec2 = egui::vec2(112.0, BTN_HEIGHT);

/// Hero splash login button.
pub const HERO_BTN: egui::Vec2 = egui::vec2(240.0, 44.0);

pub const ITEM_SPACING: Vec2 = egui::vec2(12.0, 12.0);
pub const BUTTON_PADDING: Vec2 = egui::vec2(16.0, 10.0);
pub const WINDOW_MARGIN: i8 = 20;
pub const CARD_RADIUS: u8 = 14;
pub const CARD_MARGIN: i8 = 16;

/// Hover and unhover on library cards ease over this many seconds.
pub const HOVER_EASE_SECS: f32 = 0.3;

/// Black overlay on a selected game's window art (~55%).
pub const GAME_BG_DIM: u8 = 140;

/// #0B0C11
pub const AMBIENT_TOP: Color32 = Color32::from_rgb(11, 12, 17);
/// #161922
pub const AMBIENT_BOTTOM: Color32 = Color32::from_rgb(22, 25, 34);
/// #00E5FF — progress only, not selection.
pub const CYAN: Color32 = Color32::from_rgb(0, 229, 255);

pub const TEXT_HEADER: Color32 = Color32::from_rgb(255, 255, 255);

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

pub fn text_body() -> Color32 {
    rgba(255, 255, 255, 215)
}

pub fn text_meta() -> Color32 {
    rgba(255, 255, 255, 128)
}

fn glass_fill() -> Color32 {
    rgba(24, 28, 38, 112)
}

/// Same hue as [`glass_fill`], a small brightness step up.
fn raised_fill() -> Color32 {
    let base = glass_fill();
    rgba(
        base.r().saturating_add(26),
        base.g().saturating_add(26),
        base.b().saturating_add(26),
        base.a(),
    )
}

fn emphasis_fill(hovered: bool) -> Color32 {
    let add = if hovered { 52 } else { 38 };
    let base = glass_fill();
    rgba(
        base.r().saturating_add(add),
        base.g().saturating_add(add),
        base.b().saturating_add(add),
        225,
    )
}

fn control_fill() -> Color32 {
    rgba(30, 34, 44, 168)
}

/// Top-level panels stay under 0.75 alpha so the background reads through.
fn panel_fill() -> Color32 {
    rgba(14, 16, 24, 160)
}

fn modal_fill() -> Color32 {
    rgba(18, 22, 32, 235)
}

fn invisible_stroke() -> Stroke {
    Stroke::new(1.0_f32, Color32::TRANSPARENT)
}

const CHECK_FILL: Color32 = Color32::from_rgb(28, 32, 44);
const CHECK_HOVER: Color32 = Color32::from_rgb(44, 50, 70);
const CHECK_ACTIVE: Color32 = Color32::from_rgb(54, 60, 74);

pub const BRAND: &str = "gog-conjure";
pub const TAGLINE: &str = "Summon your GOG library to disk.";

/// Placeholder kept so a later paint can commit under the frame fill.
pub struct WashSlot(egui::layers::ShapeIdx);

pub fn apply(ctx: &egui::Context) {
    let mut style = Style::default();

    style.spacing.item_spacing = ITEM_SPACING;
    style.spacing.button_padding = BUTTON_PADDING;
    style.spacing.interact_size = egui::vec2(48.0, BTN_HEIGHT);
    style.spacing.window_margin = Margin::same(WINDOW_MARGIN);
    style.spacing.menu_margin = Margin::same(10);
    style.visuals = visuals();

    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(26.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(15.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(15.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();

    ctx.set_style(style);
}

pub fn paint_ambient_background(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::background());
    let mut mesh = Mesh::default();
    add_quad(
        &mut mesh,
        rect,
        AMBIENT_TOP,
        Color32::from_rgb(14, 16, 22),
        Color32::from_rgb(18, 21, 30),
        AMBIENT_BOTTOM,
    );
    let reach = rect.width().max(rect.height());
    add_radial(
        &mut mesh,
        Pos2::new(
            rect.min.x + rect.width() * 0.18,
            rect.min.y + rect.height() * 0.08,
        ),
        reach * 0.5,
        reach * 0.36,
        premul(255, 255, 255, 16),
    );
    painter.add(Shape::mesh(mesh));
}

/// What the blurred card backdrop is a copy of.
pub enum BackdropSource<'a> {
    Gradient,
    Cover(&'a Arc<ColorImage>),
}

#[derive(Clone)]
struct BackdropCache {
    key: BackdropKey,
    tex: egui::TextureHandle,
    /// Size last observed while waiting out a resize. The built texture can lag this.
    seen_w: u32,
    seen_h: u32,
    seen_at: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct BackdropKey {
    w: u32,
    h: u32,
    source: u64,
}

/// Identity of a finished blur. Compared so a stale worker result is dropped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlurKey {
    pub w: u32,
    pub h: u32,
    pub source: u64,
}

/// Crossfade between two already-blurred window textures.
const BACKDROP_FADE_SECS: f64 = 0.42;

/// Wait this long after the last size change before rebuilding the blur.
const BACKDROP_RESIZE_SETTLE_SECS: f64 = 0.15;

fn backdrop_id() -> Id {
    Id::new("gog_backdrop_blur")
}

fn backdrop_ctrl_id() -> Id {
    Id::new("gog_backdrop_ctrl")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackdropPlan {
    /// Texture already matches the window and the source.
    Ready,
    /// Build now (first frame, or the cover changed, or the size has settled).
    Rebuild,
    /// Keep the previous texture and check again shortly.
    Defer,
}

fn plan_backdrop(
    has_cache: bool,
    source_changed: bool,
    size_matches_cache: bool,
    seen_size_matches: bool,
    elapsed: f64,
) -> BackdropPlan {
    if !has_cache || source_changed {
        return BackdropPlan::Rebuild;
    }
    if size_matches_cache {
        return BackdropPlan::Ready;
    }
    if !seen_size_matches || elapsed < BACKDROP_RESIZE_SETTLE_SECS {
        BackdropPlan::Defer
    } else {
        BackdropPlan::Rebuild
    }
}

/// Pixel work for one backdrop. Run this on a worker; the UI thread only uploads the result.
pub struct BlurJob {
    pub key: BlurKey,
    w: usize,
    h: usize,
    sigma: f32,
    kind: BlurKind,
}

enum BlurKind {
    Gradient {
        nw: Color32,
        ne: Color32,
        se: Color32,
        sw: Color32,
    },
    Cover {
        image: Arc<ColorImage>,
        uv: Rect,
        dim: u8,
    },
}

impl BlurJob {
    pub fn render(self) -> ColorImage {
        let mut rgba = match self.kind {
            BlurKind::Gradient { nw, ne, se, sw } => {
                backdrop::raster_gradient(self.w, self.h, nw, ne, se, sw)
            }
            BlurKind::Cover { image, uv, dim } => {
                backdrop::raster_cover(image.as_ref(), self.w, self.h, uv, dim)
            }
        };
        backdrop::gaussian_blur(&mut rgba, self.w, self.h, self.sigma);
        backdrop::to_color_image(self.w, self.h, &rgba)
    }
}

#[derive(Clone)]
struct ShownBackdrop {
    tex: egui::TextureHandle,
    source: u64,
}

#[derive(Clone)]
struct BackdropCtrl {
    wanted: BlurKey,
    pending: Option<BlurKey>,
    shown: Option<ShownBackdrop>,
    fade_from: Option<ShownBackdrop>,
    fade_to: Option<ShownBackdrop>,
    fade_start: f64,
}

impl Default for BackdropCtrl {
    fn default() -> Self {
        Self {
            wanted: BlurKey {
                w: 0,
                h: 0,
                source: 0,
            },
            pending: None,
            shown: None,
            fade_from: None,
            fade_to: None,
            fade_start: 0.0,
        }
    }
}

/// Decide whether a blur should be rebuilt. Returns a job for a worker; does not touch pixels.
pub fn sync_backdrop(ctx: &egui::Context, source: BackdropSource<'_>) -> Option<BlurJob> {
    let screen = ctx.screen_rect();
    if screen.width() < 2.0 || screen.height() < 2.0 {
        return None;
    }
    let (w, h) = backdrop::half_size(screen.width(), screen.height());
    let source_id = match &source {
        BackdropSource::Gradient => 0,
        BackdropSource::Cover(img) => Arc::as_ptr(img) as u64,
    };
    let key = BackdropKey {
        w: w as u32,
        h: h as u32,
        source: source_id,
    };
    let blur_key = BlurKey {
        w: key.w,
        h: key.h,
        source: source_id,
    };
    let now = ctx.input(|i| i.time);
    let cached = ctx.data(|data| data.get_temp::<BackdropCache>(backdrop_id()));
    let mut ctrl = ctx
        .data(|data| data.get_temp::<BackdropCtrl>(backdrop_ctrl_id()))
        .unwrap_or_default();
    ctrl.wanted = blur_key;
    let plan = plan_backdrop(
        cached.is_some(),
        cached
            .as_ref()
            .is_some_and(|cache| cache.key.source != source_id),
        cached
            .as_ref()
            .is_some_and(|cache| cache.key.w == key.w && cache.key.h == key.h),
        cached
            .as_ref()
            .is_some_and(|cache| cache.seen_w == key.w && cache.seen_h == key.h),
        cached
            .as_ref()
            .map(|cache| now - cache.seen_at)
            .unwrap_or(0.0),
    );
    let job = match plan {
        BackdropPlan::Ready => None,
        BackdropPlan::Defer => {
            let mut cache = cached.expect("defer only happens with a cache");
            if cache.seen_w != key.w || cache.seen_h != key.h {
                cache.seen_w = key.w;
                cache.seen_h = key.h;
                cache.seen_at = now;
            }
            let elapsed = (now - cache.seen_at).max(0.0);
            let wait = (BACKDROP_RESIZE_SETTLE_SECS - elapsed).max(0.016);
            ctx.data_mut(|data| data.insert_temp(backdrop_id(), cache));
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
            None
        }
        BackdropPlan::Rebuild => {
            if ctrl.pending == Some(blur_key) {
                None
            } else {
                ctrl.pending = Some(blur_key);
                Some(blur_job(w, h, screen, source, blur_key))
            }
        }
    };
    ctx.data_mut(|data| data.insert_temp(backdrop_ctrl_id(), ctrl));
    job
}

fn blur_job(w: usize, h: usize, screen: Rect, source: BackdropSource<'_>, key: BlurKey) -> BlurJob {
    let sigma = backdrop::downscaled_sigma(screen.width(), w);
    let kind = match source {
        BackdropSource::Gradient => BlurKind::Gradient {
            nw: AMBIENT_TOP,
            ne: Color32::from_rgb(14, 16, 22),
            se: Color32::from_rgb(18, 21, 30),
            sw: AMBIENT_BOTTOM,
        },
        BackdropSource::Cover(img) => {
            let uv = cover_uv(screen, Vec2::new(img.size[0] as f32, img.size[1] as f32));
            BlurKind::Cover {
                image: Arc::clone(img),
                uv,
                dim: GAME_BG_DIM,
            }
        }
    };
    BlurJob {
        key,
        w,
        h,
        sigma,
        kind,
    }
}

/// Upload one finished blur. A result that is no longer the wanted size or source is ignored.
pub fn install_backdrop(ctx: &egui::Context, key: BlurKey, image: ColorImage) {
    let mut ctrl = ctx
        .data(|data| data.get_temp::<BackdropCtrl>(backdrop_ctrl_id()))
        .unwrap_or_default();
    if ctrl.pending == Some(key) {
        ctrl.pending = None;
    }
    if ctrl.wanted != key {
        ctx.data_mut(|data| data.insert_temp(backdrop_ctrl_id(), ctrl));
        return;
    }
    let tex = ctx.load_texture("backdrop-blur", image, TextureOptions::LINEAR);
    let now = ctx.input(|i| i.time);
    let shown = ShownBackdrop {
        tex: tex.clone(),
        source: key.source,
    };
    if let Some(prev) = ctrl.shown.clone() {
        if prev.source != key.source {
            ctrl.fade_from = Some(prev);
            ctrl.fade_to = Some(shown.clone());
            ctrl.fade_start = now;
        } else if ctrl.fade_to.is_some() {
            ctrl.fade_to = Some(shown.clone());
        }
    }
    ctrl.shown = Some(shown);
    ctx.data_mut(|data| {
        data.insert_temp(
            backdrop_id(),
            BackdropCache {
                key: BackdropKey {
                    w: key.w,
                    h: key.h,
                    source: key.source,
                },
                tex,
                seen_w: key.w,
                seen_h: key.h,
                seen_at: now,
            },
        );
        data.insert_temp(backdrop_ctrl_id(), ctrl);
    });
    ctx.request_repaint();
}

pub fn cancel_backdrop(ctx: &egui::Context, key: BlurKey) {
    let Some(mut ctrl) = ctx.data(|data| data.get_temp::<BackdropCtrl>(backdrop_ctrl_id())) else {
        return;
    };
    if ctrl.pending == Some(key) {
        ctrl.pending = None;
        ctx.data_mut(|data| data.insert_temp(backdrop_ctrl_id(), ctrl));
    }
}

pub fn backdrop_is_showing(ctx: &egui::Context) -> bool {
    ctx.data(|data| {
        data.get_temp::<BackdropCtrl>(backdrop_ctrl_id())
            .is_some_and(|ctrl| ctrl.shown.is_some())
    })
}

/// Paint the settled blurred texture across the window, crossfading when the source changes.
pub fn paint_window_backdrop(ctx: &egui::Context) {
    let rect = ctx.screen_rect();
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let now = ctx.input(|i| i.time);
    let Some(mut ctrl) = ctx.data(|data| data.get_temp::<BackdropCtrl>(backdrop_ctrl_id())) else {
        paint_ambient_background(ctx);
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::background());
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    if let (Some(from), Some(to)) = (ctrl.fade_from.clone(), ctrl.fade_to.clone()) {
        let t = ((now - ctrl.fade_start) / BACKDROP_FADE_SECS).clamp(0.0, 1.0) as f32;
        let eased = smoothstep(t);
        painter.image(from.tex.id(), rect, uv, Color32::WHITE);
        painter.image(
            to.tex.id(),
            rect,
            uv,
            Color32::from_white_alpha((eased * 255.0).round() as u8),
        );
        if t >= 1.0 {
            ctrl.fade_from = None;
            ctrl.fade_to = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        ctx.data_mut(|data| data.insert_temp(backdrop_ctrl_id(), ctrl));
    } else if let Some(shown) = &ctrl.shown {
        painter.image(shown.tex.id(), rect, uv, Color32::WHITE);
    } else {
        paint_ambient_background(ctx);
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn chrome_frame() -> Frame {
    floating_panel(Margin::same(WINDOW_MARGIN))
}

pub fn content_frame() -> Frame {
    floating_panel(Margin::same(WINDOW_MARGIN))
}

/// Full-height library column. No outer gap, so it meets the window edges.
pub fn sidebar_frame() -> Frame {
    Frame::new()
        .fill(panel_fill())
        .stroke(invisible_stroke())
        .corner_radius(CornerRadius::ZERO)
        .inner_margin(Margin::same(WINDOW_MARGIN))
        .outer_margin(Margin::ZERO)
}

pub struct GlassFrame {
    frame: Frame,
    radius: u8,
}

impl GlassFrame {
    pub fn show<R>(
        self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        let slot = ui.painter().add(Shape::Noop);
        let inner = self.frame.show(ui, add_contents);
        paint_card_backdrop(ui, slot, inner.response.rect, self.radius);
        inner
    }
}

pub fn glass_card(hovered: bool, selected: bool) -> GlassFrame {
    glass_frame(hovered, selected, Margin::same(CARD_MARGIN), CARD_RADIUS)
}

pub fn glass_row(hovered: bool, selected: bool) -> GlassFrame {
    glass_frame(hovered, selected, Margin::symmetric(14, 12), 12)
}

/// Width of the square select strip on the right of a library row.
pub const CHECK_STRIP_W: f32 = 56.0;

/// Square library card. The select strip owns hover and checked feedback.
pub fn game_row_frame() -> GlassFrame {
    GlassFrame {
        frame: Frame::new()
            .fill(glass_fill())
            .stroke(invisible_stroke())
            .corner_radius(CornerRadius::ZERO)
            .inner_margin(Margin {
                left: 18,
                right: CHECK_STRIP_W as i8,
                top: 16,
                bottom: 16,
            }),
        radius: 0,
    }
}

/// Full-height select strip along the right edge of a library row.
pub fn check_strip_rect(row: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(row.right() - CHECK_STRIP_W, row.top()),
        row.right_bottom(),
    )
}

/// Hover fill fades in over [`ease_hover`]. No corners. An unchecked mark stays dim.
pub fn paint_check_strip(painter: &Painter, rect: Rect, hover_t: f32, checked: bool) {
    let t = hover_t.clamp(0.0, 1.0);
    if t > 0.0 {
        painter.rect_filled(
            rect,
            CornerRadius::ZERO,
            lerp_color(Color32::TRANSPARENT, raised_fill(), t),
        );
    }
    let color = if checked {
        TEXT_HEADER
    } else {
        Color32::from_rgb(42, 46, 54)
    };
    paint_check(painter, rect, color);
}

/// Outer height of a library row whose inner content is `content_h` points tall.
/// Matches what [`game_row_frame`] allocates, including margin and stroke.
pub fn game_row_outer_height(content_h: f32) -> f32 {
    let frame = game_row_frame().frame;
    content_h + frame.inner_margin.sum().y + frame.outer_margin.sum().y + frame.stroke.width * 2.0
}

pub fn modal_frame() -> Frame {
    Frame::new()
        .fill(modal_fill())
        .stroke(invisible_stroke())
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(WINDOW_MARGIN))
        .shadow(Shadow {
            offset: [0, 6],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(80),
        })
}

pub fn header(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).strong().color(TEXT_HEADER)
}

pub fn meta(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).small().color(text_meta())
}

pub fn primary_button(text: impl Into<String>, hovered: bool) -> egui::Button<'static> {
    egui::Button::new(header(text))
        .fill(emphasis_fill(hovered))
        .stroke(invisible_stroke())
        .corner_radius(CornerRadius::same(8))
}

pub fn nav_button(
    label: impl Into<String>,
    selected: bool,
    hovered: bool,
) -> egui::Button<'static> {
    let fill = if selected {
        raised_fill()
    } else if hovered {
        lerp_color(control_fill(), raised_fill(), 0.7)
    } else {
        control_fill()
    };
    egui::Button::new(header(label))
        .fill(fill)
        .stroke(invisible_stroke())
        .corner_radius(CornerRadius::same(8))
}

pub fn progress(fraction: f32) -> ProgressBar {
    ProgressBar::new(fraction)
        .fill(CYAN)
        .corner_radius(CornerRadius::same(4))
}

pub fn paint_meter(painter: &Painter, rect: Rect, fraction: f32) {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    painter.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(8, 10, 16));
    let mut filled = rect;
    filled.set_width((rect.width() * fraction).max(0.0));
    if filled.width() < 1.0 {
        return;
    }
    let light = Color32::from_rgb(186, 194, 204);
    let mut mesh = Mesh::default();
    add_quad(&mut mesh, filled, light, CYAN, CYAN, light);
    painter.add(Shape::mesh(mesh));
}

pub fn paint_check(painter: &Painter, rect: Rect, color: Color32) {
    let stroke = Stroke::new(2.2_f32, color);
    let x = rect.center().x - 9.0;
    let cy = rect.center().y + 0.5;
    let a = Pos2::new(x, cy);
    let b = Pos2::new(x + 7.0, cy + 7.0);
    let c = Pos2::new(x + 18.0, cy - 8.0);
    painter.line_segment([a, b], stroke);
    painter.line_segment([b, c], stroke);
}

pub fn expand_x(ui: &mut egui::Ui) {
    ui.set_min_width(ui.available_width());
}

pub fn hover_id(ui: &egui::Ui, salt: impl std::hash::Hash) -> Id {
    ui.id().with(salt)
}

pub fn was_hovered(ui: &egui::Ui, id: Id) -> bool {
    ui.data(|data| data.get_temp::<Rect>(id))
        .is_some_and(|rect| ui.rect_contains_pointer(rect))
}

pub fn store_hover_rect(ui: &egui::Ui, id: Id, rect: Rect) {
    ui.data_mut(|data| data.insert_temp(id, rect));
}

/// Move `id`'s stored progress toward 1 while hovered and back to 0 otherwise.
pub fn ease_hover(ui: &egui::Ui, id: Id, hovered: bool) -> f32 {
    let target = if hovered { 1.0 } else { 0.0 };
    let mut t = ui.data(|data| data.get_temp::<f32>(id)).unwrap_or(0.0);
    let dt = ui.input(|i| i.stable_dt).clamp(0.0, 0.05);
    let step = dt / HOVER_EASE_SECS;
    if (t - target).abs() > f32::EPSILON {
        if t < target {
            t = (t + step).min(1.0);
        } else {
            t = (t - step).max(0.0);
        }
        if (t - target).abs() > 0.001 {
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|data| data.insert_temp(id, t));
    t
}

pub fn reserve_art_wash(ui: &egui::Ui) -> WashSlot {
    WashSlot(ui.painter().add(Shape::Noop))
}

pub fn commit_art_wash(
    ui: &egui::Ui,
    slot: WashSlot,
    rect: Rect,
    tex: &egui::TextureHandle,
    radius: u8,
) {
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    ui.painter().set(slot.0, art_wash_shape(rect, tex, radius));
}

fn floating_panel(inner: Margin) -> Frame {
    Frame::new()
        .fill(panel_fill())
        .stroke(invisible_stroke())
        .corner_radius(CornerRadius::same(14))
        .inner_margin(inner)
        .outer_margin(Margin::same(10))
}

fn glass_frame(hovered: bool, selected: bool, margin: Margin, radius: u8) -> GlassFrame {
    let fill = if selected || hovered {
        raised_fill()
    } else {
        glass_fill()
    };
    GlassFrame {
        frame: Frame::new()
            .fill(fill)
            .stroke(invisible_stroke())
            .corner_radius(CornerRadius::same(radius))
            .inner_margin(margin),
        radius,
    }
}

fn paint_card_backdrop(ui: &egui::Ui, slot: egui::layers::ShapeIdx, rect: Rect, radius: u8) {
    if rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }
    let Some(cache) = ui.data(|data| data.get_temp::<BackdropCache>(backdrop_id())) else {
        return;
    };
    let uv = backdrop::screen_uv(ui.ctx().screen_rect(), rect);
    let shape = Shape::Rect(
        egui::epaint::RectShape::filled(rect, CornerRadius::same(radius), Color32::WHITE)
            .with_texture(cache.tex.id(), uv),
    );
    ui.painter().set(slot, shape);
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.override_text_color = Some(text_body());
    v.window_fill = modal_fill();
    v.panel_fill = panel_fill();
    v.faint_bg_color = Color32::from_rgb(16, 18, 26);
    v.extreme_bg_color = Color32::from_rgb(8, 10, 16);
    v.code_bg_color = Color32::from_rgb(12, 14, 22);
    v.widgets.noninteractive = widget_visuals(
        Color32::from_rgb(20, 24, 33),
        panel_fill(),
        text_body(),
        CARD_RADIUS,
        0.0,
    );
    v.widgets.inactive = widget_visuals(CHECK_FILL, control_fill(), TEXT_HEADER, 8, 0.0);
    v.widgets.hovered = widget_visuals(CHECK_HOVER, raised_fill(), TEXT_HEADER, 8, 0.0);
    v.widgets.active = widget_visuals(CHECK_ACTIVE, raised_fill(), TEXT_HEADER, 8, 0.0);
    v.widgets.open = widget_visuals(CHECK_HOVER, raised_fill(), TEXT_HEADER, 8, 0.0);
    v.selection.bg_fill = raised_fill();
    v.selection.stroke = invisible_stroke();
    v.hyperlink_color = Color32::from_rgb(220, 226, 234);
    v.warn_fg_color = Color32::from_rgb(255, 176, 92);
    v.error_fg_color = Color32::from_rgb(255, 96, 112);
    v.window_stroke = invisible_stroke();
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(80),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(70),
    };
    v
}

fn widget_visuals(
    bg_fill: Color32,
    weak_bg_fill: Color32,
    text: Color32,
    radius: u8,
    expansion: f32,
) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill,
        weak_bg_fill,
        bg_stroke: invisible_stroke(),
        corner_radius: CornerRadius::same(radius),
        fg_stroke: Stroke::new(1.0_f32, text),
        expansion,
    }
}

fn art_wash_shape(rect: Rect, tex: &egui::TextureHandle, radius: u8) -> Shape {
    let uv = cover_uv(rect, tex.size_vec2());
    let tint = rgba(120, 124, 132, 110);
    Shape::Rect(
        egui::epaint::RectShape::filled(rect, CornerRadius::same(radius), tint)
            .with_texture(tex.id(), uv),
    )
}

fn cover_uv(dest: Rect, tex: Vec2) -> Rect {
    if dest.width() < 1.0 || dest.height() < 1.0 || tex.x < 1.0 || tex.y < 1.0 {
        return Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
    }
    let dest_aspect = dest.width() / dest.height();
    let src_aspect = tex.x / tex.y;
    if src_aspect > dest_aspect {
        let w = dest_aspect / src_aspect;
        let x = (1.0 - w) * 0.5;
        Rect::from_min_max(Pos2::new(x, 0.0), Pos2::new(x + w, 1.0))
    } else {
        let h = src_aspect / dest_aspect;
        let y = (1.0 - h) * 0.5;
        Rect::from_min_max(Pos2::new(0.0, y), Pos2::new(1.0, y + h))
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    rgba(
        lerp_u8(a.r(), b.r(), t),
        lerp_u8(a.g(), b.g(), t),
        lerp_u8(a.b(), b.b(), t),
        lerp_u8(a.a(), b.a(), t),
    )
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn add_quad(mesh: &mut Mesh, rect: Rect, nw: Color32, ne: Color32, se: Color32, sw: Color32) {
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), nw);
    mesh.colored_vertex(rect.right_top(), ne);
    mesh.colored_vertex(rect.right_bottom(), se);
    mesh.colored_vertex(rect.left_bottom(), sw);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

fn add_radial(mesh: &mut Mesh, center: Pos2, rx: f32, ry: f32, color: Color32) {
    let n = 24u32;
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(center, color);
    for i in 0..=n {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(
            center + egui::vec2(t.cos() * rx, t.sin() * ry),
            Color32::TRANSPARENT,
        );
    }
    for i in 0..n {
        mesh.add_triangle(base, base + 1 + i, base + 2 + i);
    }
}

/// Mesh vertices are premultiplied. Opaque gradient stops do not need this.
fn premul(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    let f = a as f32 / 255.0;
    Color32::from_rgba_premultiplied(
        (r as f32 * f).round() as u8,
        (g as f32 * f).round() as u8,
        (b as f32 * f).round() as u8,
        a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glass_tokens_match_the_spec() {
        assert_eq!(AMBIENT_TOP, Color32::from_rgb(11, 12, 17));
        assert_eq!(AMBIENT_BOTTOM, Color32::from_rgb(22, 25, 34));
        assert_eq!(CYAN, Color32::from_rgb(0, 229, 255));
        assert!(panel_fill().a() <= 191, "panel alpha must be <= 0.75");
        assert!(glass_fill().a() < 160, "card fill should stay translucent");
        assert_eq!(invisible_stroke().color, Color32::TRANSPARENT);
        assert!(modal_fill().a() >= 230 && modal_fill().a() <= 240);
        assert_eq!(TEXT_HEADER, Color32::from_rgb(255, 255, 255));
        assert_eq!(text_body().a(), 215);
        assert_eq!(text_meta().a(), 128);
        assert!(ITEM_SPACING.x >= 12.0 && ITEM_SPACING.y >= 12.0);
        assert_eq!(BUTTON_PADDING, egui::vec2(16.0, 10.0));
        assert_eq!(WINDOW_MARGIN, 20);
        assert_eq!(CARD_MARGIN, 16);
        assert_eq!(CARD_RADIUS, 14);
        assert_eq!(HOVER_EASE_SECS, 0.3);
        assert!((102..=153).contains(&GAME_BG_DIM));

        let idle = glass_fill();
        let hot = raised_fill();
        let dr = hot.r() as i16 - idle.r() as i16;
        let dg = hot.g() as i16 - idle.g() as i16;
        let db = hot.b() as i16 - idle.b() as i16;
        assert!(dr > 0 && dg > 0 && db > 0);
        assert!((dr - dg).abs() <= 4 && (dg - db).abs() <= 4);
        assert!(hot.b() < 120, "raised fill must not be violet");
        assert_eq!(hot.a(), idle.a());
    }

    #[test]
    fn resize_defers_blur_until_the_size_settles() {
        assert_eq!(
            plan_backdrop(false, false, false, false, 0.0),
            BackdropPlan::Rebuild
        );
        assert_eq!(
            plan_backdrop(true, true, false, false, 0.0),
            BackdropPlan::Rebuild,
            "a new cover rebuilds immediately"
        );
        assert_eq!(
            plan_backdrop(true, false, true, true, 0.0),
            BackdropPlan::Ready
        );
        assert_eq!(
            plan_backdrop(true, false, false, false, 0.0),
            BackdropPlan::Defer,
            "a fresh size change keeps the previous texture"
        );
        assert_eq!(
            plan_backdrop(true, false, false, true, 0.05),
            BackdropPlan::Defer
        );
        assert_eq!(
            plan_backdrop(true, false, false, true, BACKDROP_RESIZE_SETTLE_SECS),
            BackdropPlan::Rebuild
        );
    }

    #[test]
    fn cover_uv_crops_instead_of_stretching() {
        let dest = Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 100.0));
        let wide = cover_uv(dest, egui::vec2(200.0, 100.0));
        assert!((wide.width() - 0.5).abs() < 0.001);
        assert!((wide.min.x - 0.25).abs() < 0.001);
        assert!((wide.height() - 1.0).abs() < 0.001);

        let tall = cover_uv(dest, egui::vec2(100.0, 200.0));
        assert!((tall.height() - 0.5).abs() < 0.001);
        assert!((tall.min.y - 0.25).abs() < 0.001);
        assert!((tall.width() - 1.0).abs() < 0.001);
    }

    #[test]
    fn backdrop_crossfade_is_under_half_a_second() {
        assert!((0.35..=0.5).contains(&BACKDROP_FADE_SECS));
        assert!((smoothstep(0.0) - 0.0).abs() < 1e-5);
        assert!((smoothstep(1.0) - 1.0).abs() < 1e-5);
        assert!((smoothstep(0.5) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn blur_job_renders_without_a_ui_context() {
        let job = BlurJob {
            key: BlurKey {
                w: 8,
                h: 4,
                source: 0,
            },
            w: 8,
            h: 4,
            sigma: 1.0,
            kind: BlurKind::Gradient {
                nw: AMBIENT_TOP,
                ne: AMBIENT_TOP,
                se: AMBIENT_BOTTOM,
                sw: AMBIENT_BOTTOM,
            },
        };
        let image = job.render();
        assert_eq!(image.size, [8, 4]);
        assert_eq!(image.pixels.len(), 32);
    }
}
