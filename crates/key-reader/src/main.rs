//! `camerata-key-reader`: the stable keychain reader (issue #574).
//!
//! It reads ONE keychain item, service `camerata`, account `anthropic_api_key`, through the
//! Security framework (not `/usr/bin/security`), and writes the value to stdout with no
//! newline. The engine reads that pipe. The key is never put in an env var, an argument, a
//! file or a log line by this program.
//!
//! It takes no arguments and no environment input: the service and account are compile-time
//! constants, so nothing a caller passes can make it read another item.
//!
//! Exit status: 0 with the key on stdout; 2 when there is no such entry (or it is blank); 1 for
//! anything else (usage, locked or denied keychain). It prints nothing on stdout on failure.

use std::io::Write;
use std::process::ExitCode;

/// The one keychain service this program reads.
const SERVICE: &str = "camerata";
/// The one keychain account this program reads.
const ACCOUNT: &str = "anthropic_api_key";

/// The program accepts no arguments at all: a caller can never choose the service or account.
fn args_are_allowed(args: &[String]) -> bool {
    args.is_empty()
}

/// Why no key came back.
#[derive(Debug, PartialEq, Eq)]
enum Miss {
    /// The keychain has no such entry (or it is blank): exit 2.
    NoEntry,
    /// Locked, denied, cancelled or any other failure: exit 1.
    Other,
}

/// `errSecItemNotFound`.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

fn classify_error(code: i32) -> Miss {
    if code == ERR_SEC_ITEM_NOT_FOUND {
        Miss::NoEntry
    } else {
        Miss::Other
    }
}

#[cfg(target_os = "macos")]
fn read_item(service: &str, account: &str) -> Result<Vec<u8>, Miss> {
    security_framework::passwords::get_generic_password(service, account)
        .map_err(|e| classify_error(e.code()))
}

#[cfg(not(target_os = "macos"))]
fn read_item(_service: &str, _account: &str) -> Result<Vec<u8>, Miss> {
    Err(Miss::Other)
}

/// A usable key: valid UTF-8, not blank (blank counts as no entry).
fn usable(bytes: Vec<u8>) -> Result<String, Miss> {
    let s = String::from_utf8(bytes).map_err(|_| Miss::Other)?;
    if s.trim().is_empty() {
        Err(Miss::NoEntry)
    } else {
        Ok(s)
    }
}

fn exit_code(m: &Miss) -> u8 {
    match m {
        Miss::NoEntry => 2,
        Miss::Other => 1,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args_are_allowed(&args) {
        eprintln!("camerata-key-reader: takes no arguments");
        return ExitCode::from(1);
    }
    match read_item(SERVICE, ACCOUNT).and_then(usable) {
        Ok(key) => {
            let mut out = std::io::stdout().lock();
            if out
                .write_all(key.as_bytes())
                .and_then(|()| out.flush())
                .is_ok()
            {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(m) => ExitCode::from(exit_code(&m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_and_account_are_the_fixed_camerata_entry() {
        assert_eq!(SERVICE, "camerata");
        assert_eq!(ACCOUNT, "anthropic_api_key");
    }

    #[test]
    fn any_argument_is_refused_so_the_item_cannot_be_chosen() {
        assert!(args_are_allowed(&[]));
        for a in [
            vec!["--service".to_string(), "other".to_string()],
            vec!["github_token".to_string()],
            vec![String::new()],
        ] {
            assert!(!args_are_allowed(&a), "{a:?} must be refused");
        }
    }

    #[test]
    fn a_blank_or_non_utf8_item_is_not_a_key() {
        assert_eq!(usable(b"  \n".to_vec()), Err(Miss::NoEntry));
        assert_eq!(usable(vec![0xff, 0xfe]), Err(Miss::Other));
        assert_eq!(usable(b"sk-ant-x".to_vec()).as_deref(), Ok("sk-ant-x"));
    }

    #[test]
    fn no_such_entry_exits_two_and_everything_else_exits_one() {
        assert_eq!(classify_error(-25300), Miss::NoEntry);
        // errSecInteractionNotAllowed (locked), errSecAuthFailed, user cancelled.
        for c in [-25308, -25293, -128, 0] {
            assert_eq!(classify_error(c), Miss::Other, "{c}");
        }
        assert_eq!(exit_code(&Miss::NoEntry), 2);
        assert_eq!(exit_code(&Miss::Other), 1);
    }
}
