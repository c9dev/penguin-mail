//! The app draws its icons for Adwaita and always uses it. Another
//! desktop theme, such as Ubuntu's Yaru, swaps some of the symbolic
//! icons for its own and leaves the rest to Adwaita, so the toolbar
//! mixes two styles. A snap or a Flatpak may not see the desktop's theme
//! at all (Cinnamon names Mint-Y over XSETTINGS, which is not inside the
//! snap), and then every button but GTK's own shows a broken picture.

/// The theme the app's icons are drawn for, and the one every package
/// carries.
pub const THEME: &str = "Adwaita";

#[cfg(test)]
mod tests {
    /// Icons GTK carries in its own resources, so they show under every
    /// theme. Adwaita 48, as Debian 13 ships it, dropped the first.
    const GTK_CARRIES: &[&str] = &["emblem-system-symbolic"];

    /// Every `-symbolic` name in the app's code that neither Adwaita, GTK
    /// nor the app's own icons hold. libadwaita draws its own `adw-` icons.
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
            .filter(|name| !GTK_CARRIES.contains(&name.as_str()))
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
