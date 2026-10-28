//! The dock's tab viewer, and the viewport tab: the camera's render texture
//! drawn as an egui image, with pointer state read off its `Response` into
//! [`ViewportInput`].
//!
//! Why the response and not bevy_egui's "wants pointer input": with the whole
//! window docked the pointer is *always* over egui, so that flag can't tell
//! "over the 3D view" from "over a panel". The image's own hover/click/drag
//! state can.

use bevy::prelude::*;
use bevy_egui::egui::{self, PointerButton, Sense, load::SizedTexture};
use egui_dock::TabViewer;

use super::{EditorTab, Panels, inspector, outliner, tool_panel};
use crate::editor::picking::{ViewportInput, cursor_to_texture_px};

pub struct EditorTabs<'a, 'w, 's> {
    viewport: &'a mut ViewportInput,
    /// Whether a look-drag was in progress last pass. While the cursor is
    /// locked egui's pointer position freezes, so hover alone can't be
    /// trusted to keep the drag alive.
    was_looking: bool,
    texture: Option<egui::TextureId>,
    pixels_per_point: f32,
    panels: &'a mut Panels<'w, 's>,
}

impl<'a, 'w, 's> EditorTabs<'a, 'w, 's> {
    pub fn new(
        viewport: &'a mut ViewportInput,
        was_looking: bool,
        texture: Option<egui::TextureId>,
        pixels_per_point: f32,
        panels: &'a mut Panels<'w, 's>,
    ) -> Self {
        Self {
            viewport,
            was_looking,
            texture,
            pixels_per_point,
            panels,
        }
    }

    fn viewport_tab(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let ppp = self.pixels_per_point;
        let size = rect.size();
        self.viewport.size_px = UVec2::new(
            (size.x * ppp).round().max(1.0) as u32,
            (size.y * ppp).round().max(1.0) as u32,
        );

        let response = match self.texture {
            Some(id) => ui.add(
                egui::Image::from_texture(SizedTexture::new(id, size))
                    .fit_to_exact_size(size)
                    .sense(Sense::click_and_drag()),
            ),
            None => ui.allocate_response(size, Sense::click_and_drag()),
        };

        // Fly lock means "as if the right button were held": the cursor lock,
        // the fly camera, and pointer ownership all key off `look_active`.
        let fly_locked = self.panels.fly_lock.0;
        let secondary_down = ui.input(|i| i.pointer.secondary_down());
        let look = fly_locked
            || (secondary_down
                && (response.hovered()
                    || response.dragged_by(PointerButton::Secondary)
                    || self.was_looking));

        let rect_min = Vec2::new(response.rect.min.x, response.rect.min.y);
        let rect_size = Vec2::new(response.rect.width(), response.rect.height());
        let pointer = response
            .hover_pos()
            .or_else(|| look.then(|| ui.input(|i| i.pointer.latest_pos())).flatten());
        let cursor_px = pointer
            .and_then(|p| cursor_to_texture_px(Vec2::new(p.x, p.y), rect_min, rect_size, ppp));

        self.viewport.hovered = response.hovered();
        self.viewport.look_active = look;
        self.viewport.cursor_px = cursor_px;
        // No clicks while flying: the OS cursor is hidden and frozen where the
        // lock began, so a click would select/place at a stale position.
        self.viewport.clicked_px = (!fly_locked)
            .then(|| {
                response
                    .clicked_by(PointerButton::Primary)
                    .then_some(cursor_px)
                    .flatten()
            })
            .flatten();
        // `look` rather than `hovered`: while the cursor is locked (RMB look
        // or fly lock) egui's frozen pointer may sit over an overlay widget,
        // but the wheel still means fly speed.
        self.viewport.scroll = if response.hovered() || look {
            // egui turns Shift+wheel into horizontal scrolling (x gets the
            // delta, y is zero). The editor's Shift+Scroll means "scale", so
            // fold it back into the vertical axis.
            let (d, shift) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
            if shift {
                Vec2::new(0.0, d.y + d.x)
            } else {
                Vec2::new(d.x, d.y)
            }
        } else {
            Vec2::ZERO
        };

        // Overlay in the viewport's corner: enter fly mode without holding
        // RMB (hard to hold on a trackpad). While locked only a passive hint
        // is drawn — the frozen egui pointer could be sitting on the button,
        // and a stray click must not toggle it.
        let corner = egui::Rect::from_min_size(
            egui::pos2(rect.max.x - 128.0, rect.min.y + 8.0),
            egui::vec2(120.0, 22.0),
        );
        if fly_locked {
            ui.painter().text(
                corner.right_center(),
                egui::Align2::RIGHT_CENTER,
                "Flying — Esc exits",
                egui::FontId::proportional(13.0),
                ui.visuals().strong_text_color(),
            );
        } else if ui
            .put(corner, egui::Button::new("Fly (Esc exits)"))
            .clicked()
        {
            self.panels.fly_lock.0 = true;
        }

        if self.texture.is_none() {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No render target",
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        }
    }
}

/// Standard padding for the non-viewport tabs (the dock's own body margin is
/// zeroed so the viewport can fill its tab).
pub fn padded<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .inner_margin(egui::Margin::same(8))
        .show(ui, add)
        .inner
}

impl TabViewer for EditorTabs<'_, '_, '_> {
    type Tab = EditorTab;

    fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Self::Tab) {
        match tab {
            EditorTab::Viewport => self.viewport_tab(ui),
            EditorTab::Outliner => padded(ui, |ui| outliner::draw(ui, self.panels)),
            EditorTab::Inspector => padded(ui, |ui| inspector::draw(ui, self.panels)),
            EditorTab::Tool => padded(ui, |ui| tool_panel::draw(ui, self.panels)),
        }
    }

    fn clear_background(&self, tab: &Self::Tab) -> bool {
        *tab != EditorTab::Viewport
    }

    fn scroll_bars(&self, tab: &Self::Tab) -> [bool; 2] {
        if *tab == EditorTab::Viewport {
            [false, false]
        } else {
            [false, true]
        }
    }

    fn is_closeable(&self, _tab: &Self::Tab) -> bool {
        false
    }
}
