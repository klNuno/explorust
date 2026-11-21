use super::*;

impl Scene {
    pub(super) fn appearance(&self, m: &Model, x: f32, width: f32, h: f32, hits: &mut Vec<Hit>) {
        self.text(
            tr(
                m.fr,
                "Choose a material for each explorust surface.",
                "Choisissez le matériau de chaque surface explorust.",
            ),
            &self.ui,
            rect(x, 77.0, width, 30.0),
            MUTED,
        );
        let reset = rect(x + width - 140.0, 33.0, 140.0, 32.0);
        self.control(m, reset, Action::Reset);
        self.text(
            tr(m.fr, "Restore defaults", "Rétablir par défaut"),
            &self.small,
            rect(reset.left + 12.0, reset.top + 8.0, 122.0, 24.0),
            TEXT,
        );
        hits.push(Hit {
            action: Action::Reset,
            rect: reset,
            scrolls: false,
        });
        let mut anchor = None;
        unsafe {
            self.surface.ctx.PushAxisAlignedClip(
                &rect(
                    x - 1.0,
                    TOP - 2.0,
                    width + 2.0,
                    (h - TOP - FOOTER + 2.0).max(1.0),
                ),
                D2D1_ANTIALIAS_MODE_ALIASED,
            )
        };
        for (i, part) in std::iter::once(None)
            .chain(Part::ALL.into_iter().map(Some))
            .enumerate()
        {
            let y = TOP + i as f32 * ROW - m.scroll;
            let r = rect(x, y, width, ROW - 7.0);
            let disabled = part.is_some_and(|p| !available(p));
            self.box_(
                r,
                8.0,
                if part.is_none() {
                    rgba(140, 162, 236, 0.13)
                } else {
                    CARD
                },
                Some(BORDER),
            );
            let style = part.map(|p| m.doc.get(p));
            if let Some(s) = style {
                if disabled {
                    self.icon("\u{E713}", x + 19.0, y + 15.0, MUTED);
                } else {
                    self.swatch(x + 11.0, y + 10.0, s);
                }
            } else {
                self.icon("\u{E8B7}", x + 19.0, y + 15.0, ACCENT);
            }
            let label = part.map(|p| part_label(p, m.fr)).unwrap_or(tr(
                m.fr,
                "Apply to all",
                "Appliquer à tout",
            ));
            self.text(
                label,
                &self.strong,
                rect(x + 55.0, y + 13.0, width - 213.0, 31.0),
                TEXT,
            );
            if disabled {
                self.text(
                    tr(m.fr, "Windows panel", "Panneau Windows"),
                    &self.small,
                    rect(r.right - 141.0, y + 15.0, 132.0, 24.0),
                    MUTED,
                );
                continue;
            }
            let control = rect(r.right - 154.0, y + 8.0, 144.0, 32.0);
            if let Some(s) = style.filter(|s| s.material.can_tint()) {
                let part = part.unwrap();
                let track = rect(control.left - 132.0, y + 8.0, 80.0, 32.0);
                let middle = y + 24.0;
                self.box_(
                    rect(track.left, middle - 2.0, 80.0, 4.0),
                    2.0,
                    BORDER,
                    (m.focused == Some(Action::Opacity(part))).then_some(ACCENT),
                );
                let fill = s.opacity as f32 * 0.8;
                self.box_(rect(track.left, middle - 2.0, fill, 4.0), 2.0, ACCENT, None);
                self.box_(
                    rect(track.left + fill - 5.0, middle - 5.0, 10.0, 10.0),
                    5.0,
                    ACCENT,
                    None,
                );
                self.text(
                    &format!("{}%", s.opacity),
                    &self.small,
                    rect(track.right + 9.0, y + 15.0, 43.0, 23.0),
                    MUTED,
                );
                hits.push(Hit {
                    action: Action::Opacity(part),
                    rect: track,
                    scrolls: true,
                });
            }
            let action = Action::Choose(part);
            self.control(m, control, action);
            let value = style
                .map(|s| material_label(s.material, m.fr))
                .unwrap_or(tr(m.fr, "Choose a material", "Choisir le matériau"));
            self.text(
                value,
                &self.ui,
                rect(control.left + 9.0, y + 14.0, 114.0, 24.0),
                TEXT,
            );
            self.icon("\u{E70D}", control.right - 24.0, y + 15.0, MUTED);
            hits.push(Hit {
                action,
                rect: control,
                scrolls: true,
            });
            if m.dropdown == Some(part) {
                anchor = Some((control, style.map(|s| s.material)));
            }
        }
        unsafe { self.surface.ctx.PopAxisAlignedClip() };
        if let Some((r, selected)) = anchor {
            self.picker(m, r, selected, h, hits);
        }
        let caption = if m.error {
            tr(
                m.fr,
                "Appearance settings could not be saved.",
                "Impossible d’enregistrer les réglages d’apparence.",
            )
        } else {
            tr(
                m.fr,
                "Notifications styles apply to explorust’s notification cards.",
                "Le style des notifications s’applique aux cartes explorust.",
            )
        };
        self.text(
            caption,
            &self.small,
            rect(x, h - 27.0, width, 22.0),
            if m.error {
                rgba(255, 166, 160, 1.0)
            } else {
                MUTED
            },
        );
    }

    fn picker(
        &self,
        m: &Model,
        anchor: D2D_RECT_F,
        selected: Option<Material>,
        h: f32,
        hits: &mut Vec<Hit>,
    ) {
        let height = 5.0 * 35.0 + 12.0;
        let y = if anchor.bottom + height + FOOTER > h {
            anchor.top - height - 3.0
        } else {
            anchor.bottom + 3.0
        };
        let x = anchor.right - 192.0;
        self.box_(
            rect(x, y, 192.0, height),
            8.0,
            rgba(32, 39, 60, 1.0),
            Some(BORDER),
        );
        for (i, mat) in Material::ALL.into_iter().enumerate() {
            let top = y + 6.0 + 35.0 * i as f32;
            let action = Action::Material(mat);
            let r = rect(x + 5.0, top, 182.0, 34.0);
            if selected == Some(mat) || m.hovered == Some(action) || m.focused == Some(action) {
                self.box_(
                    r,
                    6.0,
                    if selected == Some(mat) {
                        rgba(148, 171, 245, 0.18)
                    } else {
                        CONTROL
                    },
                    (m.focused == Some(action)).then_some(ACCENT),
                );
            }
            self.swatch(
                x + 11.0,
                top + 3.0,
                Style {
                    material: mat,
                    opacity: 50,
                },
            );
            self.text(
                material_label(mat, m.fr),
                &self.ui,
                rect(x + 52.0, top + 7.0, 112.0, 24.0),
                TEXT,
            );
            if selected == Some(mat) {
                self.icon("\u{E73E}", x + 164.0, top + 8.0, ACCENT);
            }
            hits.push(Hit {
                action,
                rect: r,
                scrolls: false,
            });
        }
    }
}
