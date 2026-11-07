//! Companion credentials (plan `docs/plan/2026-11-06-v06-companions.md` §3,
//! CMP-3; architecture §18.7, §18.10): generated from the OS RNG, sealed
//! with the install's [`SecretStore`], kept in `settings` rows, resealed by
//! the key rotation and never logged.
//!
//! - **Rows:** `companions.<id>.secret.<field>`, value the sealed blob
//!   ([`CredentialKey::row_key`]). No migration: the caller writes the row
//!   in the same transaction as the companion's state row (CMP-5).
//! - **Context:** `companion:<id>:<field>` ([`CredentialKey::context`]), so
//!   a sealed value copied onto another row doesn't open there.
//! - **Encodings** (decisions `docs/decisions/2026-11-07-CMP-3.md`):
//!   - qBittorrent's WebUI password: 32 random bytes as unpadded URL-safe
//!     base64 (43 characters of `A–Z a–z 0–9 - _`), nothing its config or
//!     form encoding mangles.
//!   - Prowlarr's `ApiKey`: 16 random bytes as 32 lowercase hex digits,
//!     Prowlarr's own format.
//! - **Plaintext** lives only in [`Secret`] (redacted `Debug`, no
//!   `Display`, zeroed on drop). Errors carry no secret material, and this
//!   module emits no log events.

use std::fmt;

use lodestar_integrations::secret::{SealedSecret, Secret, SecretError, SecretStore};
use zeroize::Zeroizing;

use crate::inputs::{CompanionId, InputError};

/// Every companion row in `settings` starts with this.
pub const ROW_PREFIX: &str = "companions.";
/// What marks a companion row as a credential: `companions.<id>` then this.
const SECRET_MARK: &str = "secret.";

/// Which credential of a companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// qBittorrent's WebUI password.
    WebUiPassword,
    /// Prowlarr's `ApiKey`.
    ApiKey,
}

impl Field {
    pub const ALL: [Field; 2] = [Field::WebUiPassword, Field::ApiKey];

    /// The name used in the row key and the sealing context.
    pub fn as_str(self) -> &'static str {
        match self {
            Field::WebUiPassword => "webui_password",
            Field::ApiKey => "api_key",
        }
    }

    fn parse(s: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// Checks a pasted value (guided installs, plan Q19).
    fn validate(self, value: &str) -> Result<(), InputError> {
        match self {
            Field::ApiKey => validate_api_key(value),
            Field::WebUiPassword => {
                let b = value.as_bytes();
                if !(PASTED_PASSWORD_MIN..=PASTED_PASSWORD_MAX).contains(&b.len()) {
                    return Err(InputError::Length);
                }
                // Printable ASCII; a space only inside, never at either
                // end, where a paste picks up stray whitespace.
                if !b.iter().all(|&c| (0x20..0x7f).contains(&c))
                    || b[0] == b' '
                    || b[b.len() - 1] == b' '
                {
                    return Err(InputError::Character);
                }
                Ok(())
            }
        }
    }
}

/// qBittorrent refuses WebUI passwords shorter than six characters.
const PASTED_PASSWORD_MIN: usize = 6;
const PASTED_PASSWORD_MAX: usize = 128;
const API_KEY_LEN: usize = 32;

/// A Prowlarr `ApiKey`: exactly 32 ASCII hex digits. Pasted keys may be
/// upper case and are kept as given.
pub(crate) fn validate_api_key(value: &str) -> Result<(), InputError> {
    if value.len() != API_KEY_LEN {
        return Err(InputError::Length);
    }
    if !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(InputError::Character);
    }
    Ok(())
}

/// One credential of one companion: names its row and its context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialKey {
    companion: CompanionId,
    field: Field,
}

impl CredentialKey {
    pub fn new(companion: CompanionId, field: Field) -> CredentialKey {
        CredentialKey { companion, field }
    }

    pub fn companion(&self) -> &CompanionId {
        &self.companion
    }

    pub fn field(&self) -> Field {
        self.field
    }

    /// The sealing context: `companion:<id>:<field>`.
    pub fn context(&self) -> String {
        format!("companion:{}:{}", self.companion, self.field.as_str())
    }

    /// The `settings` key: `companions.<id>.secret.<field>`.
    pub fn row_key(&self) -> String {
        format!(
            "{ROW_PREFIX}{}.{SECRET_MARK}{}",
            self.companion,
            self.field.as_str()
        )
    }

    /// Parses a row key back; `None` for anything else, including a
    /// credential row with a field this build doesn't know.
    pub fn from_row_key(key: &str) -> Option<CredentialKey> {
        let (id, field) = split_row_key(key)?;
        Some(CredentialKey {
            companion: CompanionId::parse(id).ok()?,
            field: Field::parse(field)?,
        })
    }
}

/// `companions.<id>.secret.<field>` into its id and field, both checked
/// against their character sets. A companion id holds no `.`, so the split
/// is unambiguous.
fn split_row_key(key: &str) -> Option<(&str, &str)> {
    let (id, rest) = key.strip_prefix(ROW_PREFIX)?.split_once('.')?;
    let field = rest.strip_prefix(SECRET_MARK)?;
    CompanionId::parse(id).ok()?;
    let ok = !field.is_empty()
        && field.len() <= 32
        && field
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
    ok.then_some((id, field))
}

/// Whether a `settings` key holds a companion credential: the rows that
/// exports, settings import and support bundles must never read or write,
/// like SRC-4's `sources.secret.` rows. True for any key under
/// `companions.<id>.secret`, well formed or not, so the filter fails
/// closed.
pub fn is_secret_row(key: &str) -> bool {
    key.strip_prefix(ROW_PREFIX)
        .and_then(|rest| rest.split_once('.'))
        .is_some_and(|(_, rest)| rest == "secret" || rest.starts_with(SECRET_MARK))
}

/// A sealed credential and the row it belongs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedRow {
    pub key: String,
    pub value: SealedSecret,
}

/// A freshly generated credential: the row to store, and the plaintext,
/// once, for the companion's config file or API.
#[derive(Debug)]
pub struct Issued {
    pub row: SealedRow,
    pub secret: Secret,
}

/// qBittorrent's temporary password swapped for a generated one (guided
/// installs, plan Q19): CMP-8b signs in with `temporary`, sets
/// `next.secret`, then stores `next.row`.
#[derive(Debug)]
pub struct PasswordChange {
    pub temporary: Secret,
    pub next: Issued,
}

/// Why a credential operation failed. Carries no secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// The OS RNG failed.
    Random,
    /// Sealing or opening failed (wrong key, wrong context, altered bytes).
    Sealed(SecretError),
    /// A pasted value was refused.
    Rejected(InputError),
    /// The row key isn't a companion credential this build knows.
    UnknownRow,
    /// The operation doesn't apply to this field.
    WrongField,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialError::Random => f.write_str("the system's random generator failed"),
            CredentialError::Sealed(e) => write!(f, "sealed credential: {e:?}"),
            CredentialError::Rejected(e) => write!(f, "the value was refused: it is {e}"),
            CredentialError::UnknownRow => f.write_str("not a companion credential row"),
            CredentialError::WrongField => f.write_str("not available for this credential"),
        }
    }
}

impl std::error::Error for CredentialError {}

impl From<SecretError> for CredentialError {
    fn from(e: SecretError) -> CredentialError {
        CredentialError::Sealed(e)
    }
}

/// `N` bytes from the OS RNG, zeroed on drop.
pub(crate) fn random_bytes<const N: usize>() -> Result<Zeroizing<[u8; N]>, CredentialError> {
    let mut b = Zeroizing::new([0u8; N]);
    getrandom::fill(&mut b[..]).map_err(|_| CredentialError::Random)?;
    Ok(b)
}

/// A new value for `field` from the OS RNG, encoded as its companion needs.
fn new_value(field: Field) -> Result<Secret, CredentialError> {
    Ok(Secret::new(match field {
        Field::WebUiPassword => base64(&*random_bytes::<32>()?, URL_SAFE, false),
        Field::ApiKey => hex(&*random_bytes::<16>()?),
    }))
}

/// Generates a credential and seals it for its row. Also **Regenerate**
/// (CMP-6): it never reads or returns the credential it replaces; the
/// caller writes the new row over the old one and the new plaintext into
/// the companion's config file or API.
pub fn generate(store: &SecretStore, key: &CredentialKey) -> Result<Issued, CredentialError> {
    let secret = new_value(key.field)?;
    let value = store.seal(&key.context(), &secret)?;
    Ok(Issued {
        row: SealedRow {
            key: key.row_key(),
            value,
        },
        secret,
    })
}

/// A pasted API key or temporary password (guided installs, plan Q19):
/// validated, then sealed the same way. It's never echoed back.
pub fn accept_pasted(
    store: &SecretStore,
    key: &CredentialKey,
    pasted: Secret,
) -> Result<SealedRow, CredentialError> {
    key.field
        .validate(pasted.expose())
        .map_err(CredentialError::Rejected)?;
    Ok(SealedRow {
        key: key.row_key(),
        value: store.seal(&key.context(), &pasted)?,
    })
}

/// Opens a stored credential: for CMP-8b's configuration and CMP-6's
/// password-gated reveal, nothing else.
pub fn open(
    store: &SecretStore,
    key: &CredentialKey,
    sealed: &SealedSecret,
) -> Result<Secret, CredentialError> {
    Ok(store.open(&key.context(), sealed)?)
}

/// qBittorrent's guided install (plan Q19): opens the pasted temporary
/// password and generates the one that replaces it.
pub fn replace_temporary_password(
    store: &SecretStore,
    key: &CredentialKey,
    sealed_temporary: &SealedSecret,
) -> Result<PasswordChange, CredentialError> {
    if key.field != Field::WebUiPassword {
        return Err(CredentialError::WrongField);
    }
    Ok(PasswordChange {
        temporary: open(store, key, sealed_temporary)?,
        next: generate(store, key)?,
    })
}

/// Key rotation, step 2 (`SecretStore` module docs): reseals every
/// companion credential row sealed with an old key, and returns the rows
/// to commit before the caller retires the old keys. Other rows are
/// skipped. The context comes from the row key itself, so a field added
/// later reseals too; a row under `companions.<id>.secret` whose key
/// doesn't parse fails the whole call ([`CredentialError::UnknownRow`])
/// rather than being stranded by the retirement.
pub fn reseal_rows<'a>(
    store: &SecretStore,
    rows: impl IntoIterator<Item = (&'a str, &'a SealedSecret)>,
) -> Result<Vec<SealedRow>, CredentialError> {
    let mut out = Vec::new();
    for (key, sealed) in rows {
        if !is_secret_row(key) {
            continue;
        }
        let (id, field) = split_row_key(key).ok_or(CredentialError::UnknownRow)?;
        if store.needs_reseal(sealed) {
            out.push(SealedRow {
                key: key.to_owned(),
                value: store.reseal(&format!("companion:{id}:{field}"), sealed)?,
            });
        }
    }
    Ok(out)
}

pub(crate) const STANDARD: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64 (RFC 4648 §4 or §5), encoding only.
pub(crate) fn base64(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        let digits = chunk.len() + 1;
        for i in 0..4 {
            if i < digits {
                out.push(char::from(alphabet[((n >> (18 - 6 * i)) & 63) as usize]));
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str, field: Field) -> CredentialKey {
        CredentialKey::new(CompanionId::parse(id).unwrap(), field)
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (plain, std, url) in [
            ("", "", ""),
            ("f", "Zg==", "Zg"),
            ("fo", "Zm8=", "Zm8"),
            ("foo", "Zm9v", "Zm9v"),
            ("foob", "Zm9vYg==", "Zm9vYg"),
            ("fooba", "Zm9vYmE=", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes(), STANDARD, true), std);
            assert_eq!(base64(plain.as_bytes(), URL_SAFE, false), url);
        }
        assert_eq!(base64(&[0xfb, 0xff], STANDARD, true), "+/8=");
        assert_eq!(base64(&[0xfb, 0xff], URL_SAFE, false), "-_8");
        assert_eq!(hex(&[0x00, 0x9f, 0xff]), "009fff");
    }

    #[test]
    fn row_keys_and_contexts() {
        let k = key("qbittorrent", Field::WebUiPassword);
        assert_eq!(k.row_key(), "companions.qbittorrent.secret.webui_password");
        assert_eq!(k.context(), "companion:qbittorrent:webui_password");
        assert_eq!(CredentialKey::from_row_key(&k.row_key()), Some(k));
        let k = key("prowlarr", Field::ApiKey);
        assert_eq!(k.row_key(), "companions.prowlarr.secret.api_key");
        assert_eq!(CredentialKey::from_row_key(&k.row_key()), Some(k));
        for bad in [
            "companions.prowlarr",
            "companions.prowlarr.secret.",
            "companions.prowlarr.secret.other",
            "companions.Prowlarr.secret.api_key",
            "companions..secret.api_key",
            "sources.secret.prowlarr/api_key",
            "companions.prowlarr.secretx.api_key",
        ] {
            assert_eq!(CredentialKey::from_row_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_secret_row_filter_fails_closed() {
        for yes in [
            "companions.prowlarr.secret.api_key",
            "companions.prowlarr.secret.future_field",
            "companions.prowlarr.secret",
            "companions.BAD ID.secret.x",
            "companions.x.secret.",
        ] {
            assert!(is_secret_row(yes), "{yes}");
        }
        for no in [
            "companions.prowlarr",
            "companions.prowlarr.state",
            "companions.prowlarr.secretive",
            "sources.secret.nyaa/api_key",
            "companions",
        ] {
            assert!(!is_secret_row(no), "{no}");
        }
    }

    #[test]
    fn generated_values_have_their_companion_s_format() {
        let store = SecretStore::from_key(&[7; 32]);
        let pw = generate(&store, &key("qbittorrent", Field::WebUiPassword)).unwrap();
        let s = pw.secret.expose();
        assert_eq!(s.len(), 43);
        assert!(
            s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        let api = generate(&store, &key("prowlarr", Field::ApiKey)).unwrap();
        let s = api.secret.expose();
        assert_eq!(s.len(), 32);
        assert!(s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        // Fresh every time.
        let again = generate(&store, &key("prowlarr", Field::ApiKey)).unwrap();
        assert_ne!(again.secret.expose(), s);
    }

    #[test]
    fn pasted_values_are_validated() {
        let store = SecretStore::from_key(&[7; 32]);
        let api = key("prowlarr", Field::ApiKey);
        let pw = key("qbittorrent", Field::WebUiPassword);
        assert!(accept_pasted(&store, &api, "0123456789abcdefABCDEF0123456789".into()).is_ok());
        assert!(accept_pasted(&store, &pw, "aB3dE6gH9".into()).is_ok());
        assert!(accept_pasted(&store, &pw, "has inner space".into()).is_ok());
        // A pasted password is only ever sealed, never written into a
        // config file, so INI or XML syntax in it is harmless.
        assert!(accept_pasted(&store, &pw, "[Section]<&>".into()).is_ok());
        for (k, v, e) in [
            (&api, "0123456789abcdef", InputError::Length),
            (
                &api,
                "0123456789abcdef0123456789abcdeg",
                InputError::Character,
            ),
            (
                &api,
                "0123456789abcdef0123456789abcde\n",
                InputError::Character,
            ),
            (&pw, "short", InputError::Length),
            (&pw, " leading", InputError::Character),
            (&pw, "trailing ", InputError::Character),
            (&pw, "new\nline", InputError::Character),
            (&pw, "café-password", InputError::Character),
        ] {
            let got = accept_pasted(&store, k, v.into());
            assert_eq!(got, Err(CredentialError::Rejected(e)), "{v:?}");
        }
        let long = "x".repeat(129);
        assert_eq!(
            accept_pasted(&store, &pw, long.into()),
            Err(CredentialError::Rejected(InputError::Length))
        );
    }

    #[test]
    fn a_sealed_value_opens_only_on_its_own_row() {
        let store = SecretStore::from_key(&[7; 32]);
        let a = key("prowlarr", Field::ApiKey);
        let issued = generate(&store, &a).unwrap();
        assert_eq!(
            open(&store, &a, &issued.row.value).unwrap().expose(),
            issued.secret.expose()
        );
        let b = key("prowlarr2", Field::ApiKey);
        assert!(matches!(
            open(&store, &b, &issued.row.value),
            Err(CredentialError::Sealed(SecretError::Open))
        ));
    }

    #[test]
    fn replacing_a_temporary_password() {
        let store = SecretStore::from_key(&[7; 32]);
        let k = key("qbittorrent", Field::WebUiPassword);
        let temp = accept_pasted(&store, &k, "Tmp4Pass9".into()).unwrap();
        let change = replace_temporary_password(&store, &k, &temp.value).unwrap();
        assert_eq!(change.temporary.expose(), "Tmp4Pass9");
        assert_ne!(change.next.secret.expose(), "Tmp4Pass9");
        assert_eq!(change.next.row.key, k.row_key());
        assert_eq!(
            replace_temporary_password(&store, &key("prowlarr", Field::ApiKey), &temp.value)
                .unwrap_err(),
            CredentialError::WrongField
        );
    }

    #[test]
    fn errors_and_debug_show_no_plaintext() {
        let store = SecretStore::from_key(&[7; 32]);
        let issued = generate(&store, &key("prowlarr", Field::ApiKey)).unwrap();
        let shown = format!("{issued:?}");
        assert!(!shown.contains(issued.secret.expose()), "{shown}");
        let e = accept_pasted(
            &store,
            &key("prowlarr", Field::ApiKey),
            "not-a-key-SECRETVALUE".into(),
        )
        .unwrap_err();
        assert!(!format!("{e} {e:?}").contains("SECRETVALUE"));
    }
}
