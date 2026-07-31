//! The vector icon set, embedded in the binary.
//!
//! gpui's [`svg`](gpui::svg) element resolves its `path` through the
//! [`AssetSource`] the application was built with — [`Icons`] here — and paints
//! the result as a *monochrome* sprite: resvg rasterises the file, only the
//! alpha channel survives, and the element's `text_color` supplies the colour.
//! Two things follow, and both are why these files look the way they do:
//!
//! * the colours written in an icon never reach the screen, only its coverage
//!   does, so a `fill-opacity` below `1` reads as a lighter shade of the tint —
//!   which is how the folder and panel icons get their fill from one path;
//! * the tint is whatever the *element* asks for, and unlike text it is not
//!   inherited from a parent, so a hover that recolours a button has to reach
//!   the icon through [`group_hover`](gpui::InteractiveElement::group_hover).
//!
//! The bytes come from [`include_bytes!`], not from files read at run time: a
//! release then carries its icons wherever it is unpacked, and packaging has
//! nothing extra to ship. Cargo tracks the embedded files itself, so an edited
//! icon rebuilds the crate without help from `build.rs`.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Pixels, Result, SharedString, Styled, Svg, svg};

/// A directory row in the file panel, and its parent row.
pub const FOLDER: &str = "icons/folder.svg";

/// A file row in the file panel.
pub const FILE: &str = "icons/file.svg";

/// Drawn after the name of a symbolic link, where `ls -l` writes an arrow.
pub const SYMLINK: &str = "icons/symlink.svg";

/// The file panel button that lists the current directory again.
pub const REFRESH: &str = "icons/refresh.svg";

/// The file panel button that uploads local files into the current directory.
pub const UPLOAD: &str = "icons/upload.svg";

/// The file panel button that uploads a whole local folder.
///
/// A second button rather than a second mode of the first: the platform file
/// pickers cannot offer files and folders at once everywhere, so the choice has
/// to be made before the dialog opens.
pub const UPLOAD_FOLDER: &str = "icons/upload-folder.svg";

/// The file panel button that saves the selected remote file locally.
pub const DOWNLOAD: &str = "icons/download.svg";

/// The file panel button that creates a directory in the listed one.
///
/// The same folder outline [`UPLOAD_FOLDER`] draws, carrying a plus where that
/// one carries an arrow: the two sit side by side in the toolbar, and reading
/// them as a pair is what says one adds a folder while the other sends one.
pub const NEW_FOLDER: &str = "icons/new-folder.svg";

/// The file panel button that renames the one selected entry.
pub const RENAME: &str = "icons/rename.svg";

/// The file panel button that deletes the selection.
pub const DELETE: &str = "icons/delete.svg";

/// The toolbar button that shows and hides the remote file panel.
pub const PANEL: &str = "icons/panel.svg";

/// The button at the end of the tab strip that lists every open tab.
///
/// A plain chevron rather than a stack of lines: the strip's other end already
/// carries the application menu's `☰`, and two list-shaped glyphs facing each
/// other across one toolbar would read as the same control twice. A chevron
/// says "this opens downwards", which is the one thing the button does.
pub const TAB_LIST: &str = "icons/tab-list.svg";

/// Every icon, paired with the bytes [`Icons`] hands back for it.
const ICONS: [(&str, &[u8]); 12] = [
    (FOLDER, include_bytes!("../assets/icons/folder.svg")),
    (FILE, include_bytes!("../assets/icons/file.svg")),
    (SYMLINK, include_bytes!("../assets/icons/symlink.svg")),
    (REFRESH, include_bytes!("../assets/icons/refresh.svg")),
    (UPLOAD, include_bytes!("../assets/icons/upload.svg")),
    (
        UPLOAD_FOLDER,
        include_bytes!("../assets/icons/upload-folder.svg"),
    ),
    (DOWNLOAD, include_bytes!("../assets/icons/download.svg")),
    (NEW_FOLDER, include_bytes!("../assets/icons/new-folder.svg")),
    (RENAME, include_bytes!("../assets/icons/rename.svg")),
    (DELETE, include_bytes!("../assets/icons/delete.svg")),
    (PANEL, include_bytes!("../assets/icons/panel.svg")),
    (TAB_LIST, include_bytes!("../assets/icons/tab-list.svg")),
];

/// The asset source backing every [`svg`](gpui::svg) element in the app.
///
/// Install it with [`Application::with_assets`](gpui::Application::with_assets);
/// without it gpui's default source answers every path with `None` and the
/// icons paint as nothing at all.
pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

/// A square icon, sized and tinted.
///
/// The result is still an [`Svg`], so a caller can go on styling it — which is
/// what the hover states do.
pub fn icon(path: &'static str, size: Pixels, color: Hsla) -> Svg {
    svg().size(size).flex_none().path(path).text_color(color)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_and_is_an_svg() {
        for (name, _) in ICONS {
            let bytes = Icons
                .load(name)
                .expect("loading an embedded icon cannot fail")
                .unwrap_or_else(|| panic!("{name} is missing from the asset source"));
            let text = std::str::from_utf8(&bytes).expect("an icon must be UTF-8");
            assert!(text.contains("<svg"), "{name} is not an SVG");
            assert!(
                text.contains("viewBox=\"0 0 24 24\""),
                "{name} is not 24x24"
            );
        }
    }

    #[test]
    fn an_unknown_path_is_not_an_error() {
        assert!(
            Icons
                .load("icons/nothing.svg")
                .expect("a missing asset is not a failure")
                .is_none()
        );
    }

    #[test]
    fn listing_returns_the_whole_set() {
        assert_eq!(Icons.list("icons/").unwrap().len(), ICONS.len());
    }
}
