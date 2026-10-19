//! The terminal remembers the last look picked with `t`, in
//! `$XDG_STATE_HOME/quai-dash/tui-look` (else
//! `~/.local/state/quai-dash/tui-look`): one word, `quai`, `ghost` or
//! `angel`. A look asked for this run (`--theme`, `QUAI_DASH_THEME`) wins
//! over it; it wins over the config file's `theme` and the default. The
//! browser keeps its own pick, in the browser.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::Theme;
use crate::config::{Settings, Source};

/// Where the pick lives, from `$XDG_STATE_HOME` and `$HOME`.
fn path_in(state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match state_home.filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(home.filter(|h| !h.is_empty())?).join(".local/state"),
    };
    Some(base.join("quai-dash/tui-look"))
}

/// Where the pick lives for this user.
pub fn path() -> Option<PathBuf> {
    path_in(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// The look saved at `path`; `None` if there is none or it isn't a look.
fn load(path: &Path) -> Option<Theme> {
    let mut word = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(32)
        .read_to_string(&mut word)
        .ok()?;
    Theme::from_str(word.trim(), true).ok()
}

/// Saves `theme` at `path`: a fresh file renamed over the old one, in a
/// directory only this user can read if it has to be made.
fn save(path: &Path, theme: Theme) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let tmp = dir.join(format!(".tui-look.{}", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| writeln!(f, "{}", theme.name()));
    let renamed = written.and_then(|()| std::fs::rename(&tmp, path));
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    renamed
}

/// Remembers `theme` for the next `quai-dash tui`. A failure only means
/// it isn't remembered.
pub fn remember(theme: Theme) {
    if let Some(p) = path() {
        let _ = save(&p, theme);
    }
}

/// The look `quai-dash tui` starts in, and why.
pub fn starting(s: &Settings) -> (Theme, String) {
    starting_with(s, path().and_then(|p| load(&p).map(|t| (t, p))))
}

fn starting_with(s: &Settings, saved: Option<(Theme, PathBuf)>) -> (Theme, String) {
    let src = s.source("theme");
    let asked = matches!(src, Some(Source::Flag | Source::Env));
    match saved {
        Some((t, p)) if !asked => (t, format!("remembered in {}", p.display())),
        _ => (
            s.v.theme.unwrap_or(Theme::Quai),
            src.map_or_else(|| "default".into(), ToString::to_string),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Partial;

    fn settings(flag: Option<Theme>, file: Option<Theme>) -> Settings {
        let flag = Partial {
            theme: flag,
            ..Partial::default()
        };
        let file = Partial {
            theme: file,
            ..Partial::default()
        };
        Settings::merge(
            &flag,
            &Partial::default(),
            &file,
            &Partial::default(),
            &std::collections::BTreeMap::new(),
        )
    }

    #[test]
    fn the_path_follows_xdg_then_home() {
        assert_eq!(
            path_in(Some("/s".into()), Some("/h".into())),
            Some(PathBuf::from("/s/quai-dash/tui-look"))
        );
        assert_eq!(
            path_in(Some("".into()), Some("/h".into())),
            Some(PathBuf::from("/h/.local/state/quai-dash/tui-look"))
        );
        assert_eq!(path_in(None, None), None);
    }

    #[test]
    fn a_saved_look_comes_back() {
        let dir = std::env::temp_dir().join(format!("quai-dash-look-{}", std::process::id()));
        let p = dir.join("a/b/tui-look");
        assert_eq!(load(&p), None);
        for t in [Theme::Ghost, Theme::Angel, Theme::Quai] {
            assert!(save(&p, t).is_ok());
            assert_eq!(load(&p), Some(t));
        }
        assert!(std::fs::write(&p, "  ANGEL\n").is_ok());
        assert_eq!(load(&p), Some(Theme::Angel));
        assert!(std::fs::write(&p, "chartreuse").is_ok());
        assert_eq!(load(&p), None);
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("a")).map(|m| m.permissions().mode() & 0o777);
            assert_eq!(mode.ok(), Some(0o700));
        }
        // Nothing left behind but the file itself.
        let left: Vec<_> = std::fs::read_dir(dir.join("a/b"))
            .map(|d| d.filter_map(Result::ok).map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert_eq!(left, [OsString::from("tui-look")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn this_run_beats_remembered_beats_file() {
        let saved = Some((Theme::Angel, PathBuf::from("/s/tui-look")));
        let (t, why) = starting_with(
            &settings(Some(Theme::Ghost), Some(Theme::Quai)),
            saved.clone(),
        );
        assert_eq!((t, why.as_str()), (Theme::Ghost, "flag"));
        let (t, why) = starting_with(&settings(None, Some(Theme::Ghost)), saved.clone());
        assert_eq!(
            (t, why.as_str()),
            (Theme::Angel, "remembered in /s/tui-look")
        );
        let (t, why) = starting_with(&settings(None, Some(Theme::Ghost)), None);
        assert_eq!((t, why.as_str()), (Theme::Ghost, "file"));
        let (t, _) = starting_with(&settings(None, None), saved);
        assert_eq!(t, Theme::Angel);
        let (t, why) = starting_with(&settings(None, None), None);
        assert_eq!((t, why.as_str()), (Theme::Quai, "default"));
    }
}
