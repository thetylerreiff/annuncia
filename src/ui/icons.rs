//! Stroke icons and raster art compiled into the binary, served to GPUI's `svg()` and `img()`.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, SharedString, Styled as _, Svg, px, svg};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons!(
    "camera",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "display",
    "ear-left",
    "ear-right",
    "folder",
    "gear",
    "library",
    "mic",
    "pause",
    "play",
    "restart",
    "scissors",
    "trash",
    "wand",
    "speaker",
    "transcript",
    "window",
    "x",
);

/// Raster art served to GPUI's `img()`: the app icon and the Settings sidebar texture.
const IMAGES: &[(&str, &[u8])] = &[
    ("images/icon.png", include_bytes!("../../assets/icon.png")),
    (
        "images/sidebar.png",
        include_bytes!("../../assets/images/sidebar.png"),
    ),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .chain(IMAGES)
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .chain(IMAGES)
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| (*p).into())
            .collect())
    }
}

/// `name` is a file in `assets/icons` without the extension.
pub fn icon(name: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .size(px(size))
        .flex_none()
        .text_color(color)
}
