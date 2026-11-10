use super::AvatarKind;
use std::sync::OnceLock;
use tiny_skia::{FilterQuality, IntRect, Pixmap, PixmapPaint, Transform};

pub(super) struct IdleBody {
    plate: Pixmap,
    start: usize,
}

impl IdleBody {
    pub(super) fn new(kind: AvatarKind, original: &Pixmap) -> Option<Self> {
        let (index, collar, costume_width) = match kind {
            AvatarKind::BlueJacketPresenter => (0, 0.413, 0.92),
            AvatarKind::Raven => (1, 0.420, 0.90),
            AvatarKind::RetroRobot => (2, 0.450, 0.92),
            AvatarKind::CurlyPresenter => (3, 0.636, 0.76),
            AvatarKind::BlackSuitPresenter => (4, 0.620, 0.77),
            AvatarKind::SilverPresenter => (5, 0.439, 0.78),
            _ => return None,
        };
        let width = original.width() as usize;
        let height = original.height() as usize;
        let row_size = width * 4;
        let first_row = (height as f32 * collar) as usize;
        let start = first_row * row_size;
        let body = &costumes()[index];
        let body_width = width as f32 * costume_width;
        let body_top = height as f32 * (collar - 0.027);
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
            .skip(first_row)
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
        let feather_rows = (height as f32 * 0.029).max(1.) as usize;
        let end = start + feather_rows * row_size;
        for (i, (pixel, original)) in plate.data_mut()[start..end]
            .iter_mut()
            .zip(&data[start..end])
            .enumerate()
        {
            *pixel = mix(
                *original,
                *pixel,
                (i / row_size) as f32 / feather_rows as f32,
            );
        }
        Some(Self { plate, start })
    }

    pub(super) fn draw(&self, frame: &Pixmap) -> Pixmap {
        let mut plate = self.plate.clone();
        plate.data_mut()[..self.start].copy_from_slice(&frame.data()[..self.start]);
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
