//! Whether to drop the desktop's icon theme for Adwaita. GTK takes the
//! theme's name from the desktop, and a snap or a Flatpak cannot always
//! see that theme: Cinnamon names Mint-Y or Pop over XSETTINGS, and
//! neither is inside the snap. GTK 4 then finds only the icons the app
//! and GTK carry, and every other button shows a broken picture.

/// The theme the app's icons are drawn for, and the one every package
/// carries.
pub const FALLBACK: &str = "Adwaita";

/// Icons from the theme that the app shows on its first screen. If the
/// theme GTK picked cannot give these, it cannot give the rest.
pub const PROBES: &[&str] = &["mail-message-new-symbolic", "mail-reply-sender-symbolic"];

/// Whether to set the icon theme to `FALLBACK`. `theme` is the name GTK
/// picked, and `has_icon` asks GTK's icon theme for a name.
pub fn needs_fallback(theme: &str, has_icon: impl Fn(&str) -> bool) -> bool {
    theme != FALLBACK && !PROBES.iter().all(|name| has_icon(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_with_the_icons_stays() {
        assert!(!needs_fallback("Yaru", |_| true));
    }

    #[test]
    fn a_theme_the_app_cannot_see_falls_back_to_adwaita() {
        assert!(needs_fallback("Mint-Y-Dark-Aqua", |_| false));
    }

    #[test]
    fn one_missing_icon_is_enough_to_fall_back() {
        assert!(needs_fallback("Pop", |name| name != PROBES[1]));
    }

    #[test]
    fn adwaita_is_kept_even_when_it_lacks_the_icons() {
        // Setting the same name again would change nothing, and an
        // install without Adwaita has no better theme to offer.
        assert!(!needs_fallback(FALLBACK, |_| false));
    }

    /// Every `-symbolic` name in the app's code that neither Adwaita nor
    /// the app's own icons hold. libadwaita draws its own `adw-` icons.
    fn missing_icons(adwaita: &std::path::Path) -> Vec<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut have = std::collections::HashSet::new();
        for dir in [adwaita.to_path_buf(), root.join("data/icons")] {
            collect_svgs(&dir, &mut have);
        }
        let mut names = std::collections::BTreeSet::new();
        collect_names(&root.join("src"), &mut names);
        names
            .into_iter()
            .filter(|name| !name.starts_with("adw-") && !have.contains(name))
            .collect()
    }

    fn collect_svgs(dir: &std::path::Path, out: &mut std::collections::HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_svgs(&path, out);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && let Some(stem) = name.strip_suffix(".svg")
            {
                out.insert(stem.to_owned());
            }
        }
    }

    /// Quoted names ending in `-symbolic` in every Rust file under `dir`.
    fn collect_names(dir: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_names(&path, out);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for piece in text.split('"').skip(1).step_by(2) {
                // A bare "-symbolic" is a suffix some code checks for.
                let is_name = piece.ends_with("-symbolic")
                    && !piece.starts_with('-')
                    && piece
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                if is_name {
                    out.insert(piece.to_owned());
                }
            }
        }
    }

    #[test]
    fn every_icon_the_app_names_is_in_adwaita_or_the_app() {
        let adwaita = std::path::Path::new("/usr/share/icons/Adwaita");
        if !adwaita.join("index.theme").exists() {
            eprintln!("skipping: Adwaita is not installed");
            return;
        }
        assert_eq!(missing_icons(adwaita), Vec::<String>::new());
    }
}
