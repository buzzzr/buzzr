//! First-run setup and key loading: which relay, which key, and how the key is kept.
//!
//! `resolve` is the one entry point. It takes what is already known (a relay from the command line or environment, a key from the
//! environment or from a saved file) and asks only for what is missing. All questions go through the `Io` trait, so the flow is
//! tested with scripted answers.

use crate::config::{self, Paths};
use crate::keyfile::{self, KeyFileError};
use nostr::nips::nip19::ToBech32;
use nostr::Keys;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use zeroize::Zeroizing;

pub trait Io {
    fn say(&mut self, text: &str);
    /// Ask a question; the answer is echoed.
    fn ask(&mut self, prompt: &str) -> io::Result<String>;
    /// Ask for a secret; nothing is echoed.
    fn ask_secret(&mut self, prompt: &str) -> io::Result<Zeroizing<String>>;
}

/// The real terminal. Questions go to stderr so that stdout stays clean.
pub struct Terminal;

impl Terminal {
    fn need_tty() -> io::Result<()> {
        if io::stdin().is_terminal() {
            Ok(())
        } else {
            Err(io::Error::other(
                "buzzr needs a terminal to ask for its settings. Run it in one, or set BUZZ_RELAY_URL and BUZZ_PRIVATE_KEY",
            ))
        }
    }
}

impl Io for Terminal {
    fn say(&mut self, text: &str) {
        eprintln!("{text}");
    }

    fn ask(&mut self, prompt: &str) -> io::Result<String> {
        Self::need_tty()?;
        eprint!("{prompt}");
        io::stderr().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Err(io::Error::other("input ended"));
        }
        Ok(line.trim().to_owned())
    }

    fn ask_secret(&mut self, prompt: &str) -> io::Result<Zeroizing<String>> {
        Self::need_tty()?;
        Ok(Zeroizing::new(rpassword::prompt_password(prompt)?))
    }
}

pub struct Resolved {
    pub relay: String,
    pub key: Zeroizing<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Storage {
    Encrypted,
    Plain,
    Nothing,
}

/// Work out the relay and the private key, asking for whatever is missing.
///
/// - `relay`: from `--relay` or `BUZZ_RELAY_URL`; otherwise the saved one; otherwise asked.
/// - `env_key`: from `BUZZ_PRIVATE_KEY`; otherwise a saved key file (unlocked with a password if encrypted); otherwise asked.
/// - `force`: `--setup`, ask everything again whatever is saved.
/// - `log_n`: scrypt cost used when a new key file is written.
pub fn resolve(io: &mut dyn Io, paths: &Paths, relay: Option<String>, env_key: Option<String>, force: bool, log_n: u8) -> Result<Resolved, String> {
    let given = relay.map(|r| config::normalize_relay(&r)).transpose()?;
    // a saved relay is ignored by --setup, which asks again; one given on the command line is always used
    let relay = given.or_else(|| if force { None } else { config::load_relay(paths) });
    let saved_key_present = paths.encrypted_key().exists() || paths.plain_key().exists();
    let key_given = !force && env_key.as_ref().is_some_and(|k| !k.trim().is_empty());
    let key_needed_from_user = !key_given && (force || !saved_key_present);
    if relay.is_none() || key_needed_from_user {
        intro(io, relay.is_none(), key_needed_from_user, paths);
    }

    let relay = match relay {
        Some(r) => r,
        None => {
            let r = ask_relay(io)?;
            config::save_relay(paths, &r).map_err(|e| format!("could not save {}: {e}", paths.config().display()))?;
            io.say(&format!("Relay saved to {}.\n", paths.config().display()));
            r
        }
    };

    let key = if key_given {
        Zeroizing::new(env_key.unwrap_or_default().trim().to_owned())
    } else if key_needed_from_user {
        let key = ask_key(io)?;
        store_key(io, paths, &key, log_n)?;
        key
    } else if paths.encrypted_key().exists() {
        unlock(io, paths)?
    } else {
        read_plain(paths)?
    };
    Ok(Resolved { relay, key })
}

fn intro(io: &mut dyn Io, need_relay: bool, need_key: bool, paths: &Paths) {
    io.say("Welcome to buzzr.");
    let what = match (need_relay, need_key) {
        (true, true) => "the relay to connect to and your Buzz private key",
        (true, false) => "the relay to connect to",
        _ => "your Buzz private key",
    };
    io.say(&format!("Setup needs {what}. Settings are kept in {} (run `buzzr --setup` to change them later).\n", paths.dir().display()));
}

fn ask_relay(io: &mut dyn Io) -> Result<String, String> {
    for _ in 0..3 {
        let answer = io.ask("Relay address (for example wss://chat.example.com): ").map_err(|e| e.to_string())?;
        match config::normalize_relay(&answer) {
            Ok(r) => {
                if r.starts_with("ws://") || r.starts_with("http://") {
                    io.say("Note: this address is not encrypted (ws:// or http://). That is fine on a trusted private network, not across the internet.");
                }
                return Ok(r);
            }
            Err(why) => io.say(&format!("{why}.")),
        }
    }
    Err("no usable relay address entered".into())
}

fn ask_key(io: &mut dyn Io) -> Result<Zeroizing<String>, String> {
    for _ in 0..3 {
        let answer = io.ask_secret("Private key (hex or nsec; input is hidden): ").map_err(|e| e.to_string())?;
        let key = answer.trim();
        match Keys::parse(key) {
            Ok(keys) => {
                let who = keys.public_key().to_bech32().unwrap_or_else(|_| keys.public_key().to_hex());
                io.say(&format!("That key belongs to {who}."));
                return Ok(Zeroizing::new(key.to_owned()));
            }
            Err(_) => io.say("That is not a valid private key (64 hex characters, or nsec1...)."),
        }
    }
    Err("no valid private key entered".into())
}

fn ask_storage(io: &mut dyn Io) -> Result<Storage, String> {
    io.say("\nHow should buzzr keep the key?");
    io.say("  1) Protected by a password: you type it each time buzzr starts (recommended)");
    io.say("  2) Without a password: stored as a plain file readable only by you; buzzr starts straight away");
    io.say("  3) Do not store it: buzzr asks for the key every time");
    for _ in 0..3 {
        match io.ask("Choose 1, 2 or 3 [1]: ").map_err(|e| e.to_string())?.as_str() {
            "" | "1" => return Ok(Storage::Encrypted),
            "2" => return Ok(Storage::Plain),
            "3" => return Ok(Storage::Nothing),
            _ => io.say("Please answer 1, 2 or 3."),
        }
    }
    Err("no storage choice made".into())
}

fn store_key(io: &mut dyn Io, paths: &Paths, key: &str, log_n: u8) -> Result<(), String> {
    let choice = ask_storage(io)?;
    // whatever was stored before is replaced, never left behind to be loaded instead
    let remove = |p: std::path::PathBuf| match std::fs::remove_file(&p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(format!("could not remove {}: {e}", p.display())),
        _ => Ok(()),
    };
    match choice {
        Storage::Encrypted => {
            let pass = ask_new_password(io)?;
            io.say("Deriving the encryption key (takes a few seconds)...");
            let contents = keyfile::encrypt(key.as_bytes(), pass.as_bytes(), log_n).map_err(|e| e.to_string())?;
            // check that it really opens before replacing anything
            if keyfile::decrypt(&contents, pass.as_bytes()).map_err(|e| e.to_string())?.as_slice() != key.as_bytes() {
                return Err("could not verify the encrypted file; nothing saved".into());
            }
            config::write_private(&paths.encrypted_key(), &contents).map_err(|e| format!("could not save the key: {e}"))?;
            remove(paths.plain_key())?;
            io.say(&format!("Saved to {}. Next time buzzr asks for the password.", paths.encrypted_key().display()));
        }
        Storage::Plain => {
            config::write_private(&paths.plain_key(), &format!("{key}\n")).map_err(|e| format!("could not save the key: {e}"))?;
            remove(paths.encrypted_key())?;
            io.say(&format!(
                "Saved to {} (readable only by you). Anyone who can read that file, or a backup of it, can act as you.",
                paths.plain_key().display()
            ));
        }
        Storage::Nothing => {
            remove(paths.encrypted_key())?;
            remove(paths.plain_key())?;
            io.say("Not stored. Set BUZZ_PRIVATE_KEY or type the key each time.");
        }
    }
    Ok(())
}

fn ask_new_password(io: &mut dyn Io) -> Result<Zeroizing<String>, String> {
    for _ in 0..3 {
        let pass = io.ask_secret("Choose a password (at least 8 characters; long is better): ").map_err(|e| e.to_string())?;
        if pass.chars().count() < 8 {
            io.say("That password is too short.");
            continue;
        }
        let again = io.ask_secret("Repeat the password: ").map_err(|e| e.to_string())?;
        if *pass != *again {
            io.say("The passwords do not match.");
            continue;
        }
        return Ok(pass);
    }
    Err("no password set".into())
}

fn unlock(io: &mut dyn Io, paths: &Paths) -> Result<Zeroizing<String>, String> {
    let path = paths.encrypted_key();
    let contents = std::fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    for _ in 0..3 {
        let pass = io.ask_secret("Password to unlock your Buzz key: ").map_err(|e| e.to_string())?;
        io.say("Unlocking (takes a few seconds)...");
        match keyfile::decrypt(&contents, pass.as_bytes()) {
            Ok(key) => return String::from_utf8(key.to_vec()).map(Zeroizing::new).map_err(|_| "the saved key is not text".to_owned()),
            Err(KeyFileError::WrongPassword) => io.say("Wrong password (or the file was modified)."),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err("could not unlock the key".into())
}

fn read_plain(paths: &Paths) -> Result<Zeroizing<String>, String> {
    let path = paths.plain_key();
    let mode = std::fs::metadata(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!("{} can be read by other users (mode {:o}); run: chmod 600 {}", path.display(), mode & 0o777, path.display()));
    }
    let text = Zeroizing::new(std::fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?);
    Ok(Zeroizing::new(text.trim().to_owned()))
}

/// `buzzr --forget-key`: delete every saved key file. Returns the files that were removed.
pub fn forget_key(paths: &Paths) -> io::Result<Vec<std::path::PathBuf>> {
    let mut removed = Vec::new();
    for p in [paths.encrypted_key(), paths.plain_key()] {
        match std::fs::remove_file(&p) {
            Ok(()) => removed.push(p),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const KEY: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const PW: &str = "correct horse battery staple";
    const LOG_N: u8 = keyfile::MIN_LOG_N;

    /// Answers questions from a script and records everything said and asked.
    struct Script {
        answers: VecDeque<String>,
        said: Vec<String>,
        asked: Vec<String>,
    }

    impl Script {
        fn new(answers: &[&str]) -> Script {
            Script { answers: answers.iter().map(|s| s.to_string()).collect(), said: vec![], asked: vec![] }
        }
        fn next(&mut self, prompt: &str) -> io::Result<String> {
            self.asked.push(prompt.to_owned());
            self.answers.pop_front().ok_or_else(|| io::Error::other(format!("unexpected question: {prompt}")))
        }
        fn said(&self) -> String {
            self.said.join("\n")
        }
    }

    impl Io for Script {
        fn say(&mut self, text: &str) {
            self.said.push(text.to_owned());
        }
        fn ask(&mut self, prompt: &str) -> io::Result<String> {
            self.next(prompt)
        }
        fn ask_secret(&mut self, prompt: &str) -> io::Result<Zeroizing<String>> {
            self.next(prompt).map(Zeroizing::new)
        }
    }

    fn temp_paths() -> Paths {
        Paths::new(std::env::temp_dir().join(format!("buzzr-test-{}", uuid::Uuid::new_v4())))
    }

    fn mode(p: std::path::PathBuf) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn run(io: &mut Script, paths: &Paths, relay: Option<&str>, env_key: Option<&str>, force: bool) -> Result<Resolved, String> {
        resolve(io, paths, relay.map(String::from), env_key.map(String::from), force, LOG_N)
    }

    #[test]
    fn first_run_with_a_password() {
        let paths = temp_paths();
        let mut io = Script::new(&["chat.example.com", KEY, "1", PW, PW]);
        let got = run(&mut io, &paths, None, None, false).unwrap();
        assert_eq!((got.relay.as_str(), got.key.as_str()), ("wss://chat.example.com", KEY));
        assert_eq!(config::load_relay(&paths).as_deref(), Some("wss://chat.example.com"));
        let file = std::fs::read_to_string(paths.encrypted_key()).unwrap();
        assert!(!file.contains(KEY), "the key must not be in the file");
        assert_eq!(keyfile::decrypt(&file, PW.as_bytes()).unwrap().as_slice(), KEY.as_bytes());
        assert_eq!(mode(paths.encrypted_key()), 0o600);
        assert!(!paths.plain_key().exists());
        assert!(io.said().contains("npub1"), "shows whose key it is: {}", io.said());

        // the next start needs only the password
        let mut io = Script::new(&[PW]);
        let again = run(&mut io, &paths, None, None, false).unwrap();
        assert_eq!((again.relay.as_str(), again.key.as_str()), ("wss://chat.example.com", KEY));
        assert_eq!(io.asked.len(), 1);
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn first_run_without_a_password() {
        let paths = temp_paths();
        let mut io = Script::new(&["wss://chat.example.com", KEY, "2"]);
        let got = run(&mut io, &paths, None, None, false).unwrap();
        assert_eq!(got.key.as_str(), KEY);
        assert_eq!(std::fs::read_to_string(paths.plain_key()).unwrap().trim(), KEY);
        assert_eq!(mode(paths.plain_key()), 0o600);
        assert_eq!(mode(paths.dir().to_path_buf()), 0o700);
        assert!(!paths.encrypted_key().exists());
        assert!(io.said().contains("Anyone who can read"), "warns about the plain file");

        // the next start asks nothing
        let mut io = Script::new(&[]);
        assert_eq!(run(&mut io, &paths, None, None, false).unwrap().key.as_str(), KEY);
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn the_key_can_be_left_unstored() {
        let paths = temp_paths();
        let mut io = Script::new(&["chat.example.com", KEY, "3"]);
        run(&mut io, &paths, None, None, false).unwrap();
        assert!(!paths.encrypted_key().exists() && !paths.plain_key().exists());
        assert!(config::load_relay(&paths).is_some(), "the relay is still remembered");
        // so the next start asks for the key again, but not for the relay
        let mut io = Script::new(&[KEY, "3"]);
        run(&mut io, &paths, None, None, false).unwrap();
        assert!(!io.asked[0].contains("Relay"));
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn nothing_is_asked_when_everything_is_given() {
        let paths = temp_paths();
        let mut io = Script::new(&[]);
        let got = run(&mut io, &paths, Some("wss://chat.example.com"), Some(&format!(" {KEY}\n")), false).unwrap();
        assert_eq!((got.relay.as_str(), got.key.as_str()), ("wss://chat.example.com", KEY));
        assert!(!paths.dir().exists(), "nothing is written when nothing was asked");
        assert!(io.said.is_empty());
    }

    #[test]
    fn a_relay_from_the_command_line_is_used_but_not_saved() {
        let paths = temp_paths();
        let mut io = Script::new(&[KEY, "2"]);
        let got = run(&mut io, &paths, Some("wss://other.example.com"), None, false).unwrap();
        assert_eq!(got.relay, "wss://other.example.com");
        assert_eq!(config::load_relay(&paths), None);
        assert!(io.asked.iter().all(|q| !q.contains("Relay")));
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn mistakes_are_asked_again() {
        let paths = temp_paths();
        let mut io = Script::new(&["ftp://nope", "chat.example.com", "not a key", KEY, "9", "1", "short", PW, "different password", PW, PW]);
        let got = run(&mut io, &paths, None, None, false).unwrap();
        assert_eq!((got.relay.as_str(), got.key.as_str()), ("wss://chat.example.com", KEY));
        let said = io.said();
        for expected in ["must start with", "not a valid private key", "answer 1, 2 or 3", "too short", "do not match"] {
            assert!(said.contains(expected), "missing {expected:?} in {said}");
        }
        assert!(paths.encrypted_key().exists());
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn giving_up_after_three_bad_answers() {
        let paths = temp_paths();
        let mut io = Script::new(&["x y", "ftp://a", "wss://"]);
        assert!(run(&mut io, &paths, None, None, false).is_err());
        assert!(!paths.dir().exists());
    }

    #[test]
    fn a_wrong_password_is_retried_then_refused() {
        let paths = temp_paths();
        run(&mut Script::new(&["chat.example.com", KEY, "1", PW, PW]), &paths, None, None, false).unwrap();
        let mut io = Script::new(&["nope nope nope", PW]);
        assert_eq!(run(&mut io, &paths, None, None, false).unwrap().key.as_str(), KEY);
        assert!(io.said().contains("Wrong password"));
        let mut io = Script::new(&["a", "b", "c"]);
        assert!(run(&mut io, &paths, None, None, false).is_err());
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn setup_again_replaces_the_old_storage() {
        let paths = temp_paths();
        run(&mut Script::new(&["chat.example.com", KEY, "1", PW, PW]), &paths, None, None, false).unwrap();
        // --setup: asks for everything even though it is all saved, and a key in the environment does not skip it
        let mut io = Script::new(&["new.example.com", KEY, "2"]);
        let got = run(&mut io, &paths, None, Some(KEY), true).unwrap();
        assert_eq!(got.relay, "wss://new.example.com");
        assert!(paths.plain_key().exists());
        assert!(!paths.encrypted_key().exists(), "the old password-protected file must not linger");
        assert_eq!(config::load_relay(&paths).as_deref(), Some("wss://new.example.com"));
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn a_plain_key_file_that_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let paths = temp_paths();
        run(&mut Script::new(&["chat.example.com", KEY, "2"]), &paths, None, None, false).unwrap();
        std::fs::set_permissions(paths.plain_key(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = run(&mut Script::new(&[]), &paths, None, None, false).err().expect("must refuse");
        assert!(err.contains("chmod 600"), "{err}");
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }

    #[test]
    fn forget_key_removes_both_kinds() {
        let paths = temp_paths();
        run(&mut Script::new(&["chat.example.com", KEY, "2"]), &paths, None, None, false).unwrap();
        std::fs::write(paths.encrypted_key(), "x").unwrap();
        assert_eq!(forget_key(&paths).unwrap().len(), 2);
        assert!(forget_key(&paths).unwrap().is_empty());
        assert!(config::load_relay(&paths).is_some(), "the relay setting stays");
        std::fs::remove_dir_all(paths.dir()).unwrap();
    }
}
