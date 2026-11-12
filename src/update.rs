//! Where this copy of buzzr came from, and the command that upgrades it.
//!
//! A running program is not told how it was installed, so we work it out from the path of our own executable. Homebrew keeps the
//! real binary in `<prefix>/Cellar/<formula>/<version>/bin/` and puts a symlink to it in `<prefix>/bin`, `cargo install` writes into
//! `$CARGO_HOME/bin` (`~/.cargo/bin`), and a build from a checkout runs out of `target/`. Resolving symlinks first makes the Cellar
//! check catch the linked copy as well; the same trick is used by `uv` (`install_source.rs`) and `gh` (`isUnderHomebrew`).

use std::path::{Path, PathBuf};

/// The newest release, for installs we have no command for.
pub const RELEASES_URL: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases/latest");

/// How the running binary was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallSource {
    /// `brew install buzzr`: the binary lives in the Homebrew Cellar.
    Homebrew,
    /// `cargo install --git <repository>`: the binary lives in the cargo bin directory.
    Cargo,
    /// A build out of a checkout (`cargo run`, `target/debug/buzzr`): nothing to upgrade.
    Dev,
    /// A system package, a copied file, a prefix we do not know.
    Other,
}

impl InstallSource {
    /// How this running copy was installed.
    pub fn detect() -> InstallSource {
        match std::env::current_exe() {
            Ok(exe) => InstallSource::from_path(&exe, cargo_bin_dir().as_deref()),
            Err(_) => InstallSource::Other,
        }
    }

    /// Classify an executable path. `cargo_bin` is `$CARGO_HOME/bin`, when we can work it out.
    pub fn from_path(exe: &Path, cargo_bin: Option<&Path>) -> InstallSource {
        let real = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
        if in_a_target_directory(&real) {
            return InstallSource::Dev;
        }
        if in_the_cellar(&real) {
            return InstallSource::Homebrew;
        }
        if let Some(bin) = cargo_bin {
            let bin = bin.canonicalize().unwrap_or_else(|_| bin.to_path_buf());
            if real.parent() == Some(bin.as_path()) {
                return InstallSource::Cargo;
            }
        }
        InstallSource::Other
    }

    /// The shell command that upgrades this copy, when there is one. Overwriting a file a package manager owns makes it drift
    /// (`brew doctor` complains, the next upgrade fights you), so for those we only ever name the manager's own command.
    pub fn upgrade_command(self) -> Option<&'static str> {
        match self {
            InstallSource::Homebrew => Some(concat!("brew update && brew upgrade ", env!("CARGO_PKG_NAME"))),
            // From the repository, not the registry: the crate is not on crates.io (its Buzz dependencies are git
            // dependencies, which a published crate cannot carry), so a bare `cargo install --force buzzr` would fail.
            // Once it is published this can become the registry form.
            InstallSource::Cargo => Some(concat!("cargo install --force --git ", env!("CARGO_PKG_REPOSITORY"))),
            InstallSource::Dev | InstallSource::Other => None,
        }
    }

    /// The one line the `update` command shows in the status bar.
    pub fn advice(self) -> String {
        match self.upgrade_command() {
            Some(command) => format!("upgrade with: {command}"),
            None if self == InstallSource::Dev => "this is a working copy; rebuild it from your checkout".to_owned(),
            None => format!("install the newest release: {RELEASES_URL}"),
        }
    }
}

/// `$CARGO_HOME/bin`, else `~/.cargo/bin`.
fn cargo_bin_dir() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
            Some(PathBuf::from(home).join(".cargo"))
        })?;
    Some(home.join("bin"))
}

/// `<prefix>/Cellar/<formula>/<version>/bin/buzzr`; the `<prefix>/bin` entry pointed here and was resolved above.
fn in_the_cellar(exe: &Path) -> bool {
    parts(exe).windows(2).any(|w| w[0] == "Cellar" && w[1].starts_with(env!("CARGO_PKG_NAME")))
}

/// A build straight out of a checkout, where there is nothing to upgrade.
fn in_a_target_directory(exe: &Path) -> bool {
    parts(exe).windows(2).any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
}

/// The path, one component at a time (a component that is not valid UTF-8 simply never matches).
fn parts(path: &Path) -> Vec<String> {
    path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cellar_binary_is_a_homebrew_install() {
        assert_eq!(InstallSource::from_path(Path::new("/opt/homebrew/Cellar/buzzr/0.1.0/bin/buzzr"), None), InstallSource::Homebrew);
    }

    #[test]
    fn another_formula_in_the_cellar_is_not_ours() {
        assert_eq!(InstallSource::from_path(Path::new("/opt/homebrew/Cellar/ripgrep/14.1.0/bin/rg"), None), InstallSource::Other);
    }

    #[test]
    fn the_cargo_bin_directory_is_a_cargo_install() {
        let bin = Path::new("/home/someone/.cargo/bin");
        assert_eq!(InstallSource::from_path(&bin.join("buzzr"), Some(bin)), InstallSource::Cargo);
    }

    #[test]
    fn a_binary_beside_the_cargo_bin_directory_is_not_a_cargo_install() {
        let bin = Path::new("/home/someone/.cargo/bin");
        assert_eq!(InstallSource::from_path(Path::new("/home/someone/.local/bin/buzzr"), Some(bin)), InstallSource::Other);
    }

    #[test]
    fn a_target_directory_build_is_a_checkout_build() {
        assert_eq!(InstallSource::from_path(Path::new("/src/buzzr/target/debug/buzzr"), None), InstallSource::Dev);
    }

    #[test]
    fn somewhere_else_is_unknown() {
        assert_eq!(InstallSource::from_path(Path::new("/usr/bin/buzzr"), None), InstallSource::Other);
    }

    #[test]
    fn a_known_place_names_the_command_that_upgrades_it() {
        assert_eq!(InstallSource::Homebrew.upgrade_command(), Some("brew update && brew upgrade buzzr"));
        assert_eq!(InstallSource::Cargo.upgrade_command(), Some("cargo install --force --git https://github.com/buzzzr/buzzr"));
    }

    #[test]
    fn a_checkout_build_and_an_unknown_place_have_no_command() {
        assert_eq!(InstallSource::Dev.upgrade_command(), None);
        assert_eq!(InstallSource::Other.upgrade_command(), None);
        assert!(RELEASES_URL.ends_with("/releases/latest"));
    }

    #[test]
    fn the_advice_line_names_the_command_or_the_release_page() {
        assert_eq!(InstallSource::Homebrew.advice(), "upgrade with: brew update && brew upgrade buzzr");
        assert_eq!(InstallSource::Cargo.advice(), "upgrade with: cargo install --force --git https://github.com/buzzzr/buzzr");
        assert_eq!(InstallSource::Dev.advice(), "this is a working copy; rebuild it from your checkout");
        assert_eq!(InstallSource::Other.advice(), format!("install the newest release: {RELEASES_URL}"));
    }
}
