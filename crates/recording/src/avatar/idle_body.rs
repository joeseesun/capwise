use super::AvatarKind;
use std::sync::OnceLock;
use tiny_skia::{
    FillRule, FilterQuality, IntRect, Mask, PathBuilder, Pixmap, PixmapPaint, Transform,
};

pub(super) struct IdleBody {
    plate: Pixmap,
    head_mask: Mask,
}

impl IdleBody {
    pub(super) fn new(kind: AvatarKind, original: &Pixmap) -> Option<Self> {
        let (index, costume_width, body_top, edge_y, neck): (_, _, _, _, &[[f32; 2]]) = match kind {
            AvatarKind::BlueJacketPresenter => (
                0,
                0.92,
                232.,
                210.,
                &[
                    [126., 210.],
                    [147., 216.],
                    [155., 238.],
                    [167., 251.],
                    [194., 255.],
                    [222., 251.],
                    [239., 239.],
                    [245., 216.],
                    [265., 210.],
                ],
            ),
            AvatarKind::Raven => (
                1,
                0.90,
                236.,
                201.,
                &[
                    [91., 201.],
                    [115., 213.],
                    [140., 226.],
                    [153., 246.],
                    [176., 267.],
                    [201., 276.],
                    [225., 267.],
                    [240., 249.],
                    [252., 227.],
                    [271., 210.],
                    [291., 201.],
                ],
            ),
            AvatarKind::RetroRobot => (
                2,
                0.92,
                251.,
                220.,
                &[
                    [154., 220.],
                    [170., 227.],
                    [170., 252.],
                    [192., 257.],
                    [218., 253.],
                    [218., 227.],
                    [232., 220.],
                ],
            ),
            AvatarKind::CurlyPresenter => (
                3,
                0.76,
                400.,
                363.,
                &[
                    [138., 363.],
                    [165., 370.],
                    [177., 400.],
                    [186., 416.],
                    [207., 425.],
                    [231., 417.],
                    [244., 395.],
                    [256., 373.],
                    [274., 363.],
                ],
            ),
            AvatarKind::BlackSuitPresenter => (
                4,
                0.77,
                397.,
                362.,
                &[
                    [145., 362.],
                    [173., 368.],
                    [185., 390.],
                    [198., 404.],
                    [219., 409.],
                    [241., 405.],
                    [252., 391.],
                    [268., 367.],
                    [286., 362.],
                ],
            ),
            AvatarKind::SilverPresenter => (
                5,
                0.78,
                325.,
                300.,
                &[
                    [260., 300.],
                    [287., 310.],
                    [307., 332.],
                    [319., 348.],
                    [348., 354.],
                    [372., 351.],
                    [393., 339.],
                    [405., 314.],
                    [431., 300.],
                ],
            ),
            _ => return None,
        };
        let width = original.width() as usize;
        let height = original.height() as usize;
        let row_size = width * 4;
        let body = &costumes()[index];
        let body_width = width as f32 * costume_width;
        let scale = body_width / body.width() as f32;
        let body_bottom = body_top + body.height() as f32 * scale;
        let canvas_height =
            ((body_bottom + 0.012 * height as f32).ceil() as u32).max(original.height());
        let mut plate =
            Pixmap::new(original.width(), canvas_height).expect("relaxed portrait dimensions");
        let data = original.data();
        for (y, row) in plate
            .data_mut()
            .chunks_exact_mut(row_size)
            .enumerate()
            .skip(edge_y as usize)
        {
            let left = y.min(height - 1) * row_size;
            let right = left + row_size - 4;
            for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                let t = x as f32 / (width - 1) as f32;
                for (channel, value) in pixel.iter_mut().take(3).enumerate() {
                    *value = mix(data[left + channel], data[right + channel], t);
                }
                pixel[3] = 255;
            }
        }
        plate.draw_pixmap(
            0,
            0,
            body.as_ref(),
            &PixmapPaint {
                quality: FilterQuality::Bilinear,
                ..PixmapPaint::default()
            },
            Transform::from_scale(scale, scale)
                .post_translate((width as f32 - body_width) * 0.5, body_top),
            None,
        );
        let mut outline = PathBuilder::new();
        outline.move_to(0., 0.);
        outline.line_to(0., edge_y);
        for &[x, y] in neck {
            outline.line_to(x, y);
        }
        outline.line_to(width as f32, edge_y);
        outline.line_to(width as f32, 0.);
        outline.close();
        let mut head_mask =
            Mask::new(plate.width(), plate.height()).expect("valid head mask dimensions");
        head_mask.fill_path(
            &outline.finish().expect("valid neckline"),
            FillRule::Winding,
            true,
            Transform::identity(),
        );
        Some(Self { plate, head_mask })
    }

    pub(super) fn draw(&self, frame: &Pixmap) -> Pixmap {
        let mut plate = self.plate.clone();
        plate.draw_pixmap(
            0,
            0,
            frame.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            Some(&self.head_mask),
        );
        plate
    }
}

fn costumes() -> &'static [Pixmap; 6] {
    static ATLAS: OnceLock<[Pixmap; 6]> = OnceLock::new();
    ATLAS.get_or_init(|| {
        let atlas = Pixmap::decode_png(include_bytes!("art/relaxed-costumes.png"))
            .expect("valid relaxed avatar artwork");
        let width = atlas.width() / 3;
        let height = atlas.height() / 2;
        std::array::from_fn(|index| {
            let x = index as u32 % 3 * width;
            let y = index as u32 / 3 * height;
            let cell = IntRect::from_xywh(x as i32, y as i32, width, height).expect("costume tile");
            let sprite = atlas.clone_rect(cell).expect("costume within atlas");
            trim(sprite)
        })
    })
}

fn trim(sprite: Pixmap) -> Pixmap {
    let width = sprite.width() as usize;
    let mut left = width;
    let mut right = 0;
    let mut top = sprite.height() as usize;
    let mut bottom = 0;
    for (y, row) in sprite.data().chunks_exact(width * 4).enumerate() {
        for (x, pixel) in row.chunks_exact(4).enumerate() {
            if pixel[3] > 95 {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    let bounds = IntRect::from_xywh(
        left as i32,
        top as i32,
        (right + 1 - left) as u32,
        (bottom + 1 - top) as u32,
    )
    .expect("relaxed costume silhouette");
    sprite.clone_rect(bounds).expect("costume within tile")
}

fn mix(a: u8, b: u8, t: f32) -> u8 {
    (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Color;

    #[test]
    fn split_shoulders_only_use_the_costume_plate() {
        let cases = [
            (AvatarKind::BlueJacketPresenter, 388, 605, 239, 209),
            (AvatarKind::Raven, 382, 605, 244, 196),
            (AvatarKind::RetroRobot, 387, 605, 262, 211),
            (AvatarKind::CurlyPresenter, 437, 670, 416, 363),
            (AvatarKind::BlackSuitPresenter, 441, 670, 404, 359),
            (AvatarKind::SilverPresenter, 694, 800, 341, 294),
        ];
        for (kind, width, height, shoulder_y, neck_y) in cases {
            let neutral = Pixmap::new(width, height).unwrap();
            let costume = IdleBody::new(kind, &neutral).unwrap();
            let mut animated = neutral.clone();
            animated.fill(Color::from_rgba8(255, 0, 255, 255));
            let output = costume.draw(&animated);
            for x in [width / 4, width * 3 / 4] {
                assert_eq!(
                    output.pixel(x, shoulder_y),
                    costume.plate.pixel(x, shoulder_y),
                    "{kind:?} copied the old shoulder",
                );
            }
            assert_eq!(
                output.pixel(width / 2, neck_y),
                animated.pixel(width / 2, neck_y),
                "{kind:?} lost the center of the neck",
            );
        }
    }
}
