//! Application asset source: kit defaults plus the product icons Sotto embeds.

use gpui_kit::AssetSource;
use gpui_kit::SharedString;

gpui_kit::assets::icon_assets!(SottoExtraIcons, [AppWindow, Mic, FileInput, House]);

/// Kit `Assets` plus the Lucide icons Home / capture / mic / import need.
///
/// Default `gpui_kit::assets::Assets` embeds only `default-icons.txt`. Product markers
/// (`House`, `AppWindow`, `Mic`, `FileInput`) live outside that set — register this
/// composed source at boot so those paths resolve to real SVG bytes (ADR-0025).
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(bytes) = SottoExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(SottoExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::AppAssets;
    use gpui_kit::AssetSource;
    use gpui_kit::assets::IconName;

    #[test]
    fn product_icons_load_real_svg_bytes() {
        let source = AppAssets;
        for name in [
            IconName::AppWindow,
            IconName::Mic,
            IconName::FileInput,
            IconName::House,
            IconName::Delete,
            IconName::CircleX,
        ] {
            let path = name.path();
            let loaded = source.load(path.as_ref());
            assert!(
                loaded.is_ok(),
                "{name:?} path {path} must load without error: {loaded:?}"
            );
            let bytes = loaded.ok().flatten();
            assert!(
                bytes
                    .as_ref()
                    .is_some_and(|b| b.starts_with(b"<svg") || b.starts_with(b"<?xml")),
                "{name:?} path {path} must resolve to embedded SVG bytes, got {bytes:?}"
            );
        }
    }
}
