//! The engine's version in PEP 440 form, the form the Python package carries:
//! the endpoints file, a client's declaration and the interpreter check all
//! compare this string.

/// The crate version in PEP 440 form.
pub static VERSION: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| pep440(env!("CARGO_PKG_VERSION")));

/// Why a client of version `client` (PEP 440) is refused: its version is not
/// [`VERSION`].
pub fn mismatch_message(client: &str) -> String {
    format!(
        "the client's macrocosim package is version {client}, and this engine is version \
         {}; install the package of the same version",
        *VERSION
    )
}

/// A semver version in PEP 440 form, with maturin's rules: a
/// pre-release `-alpha.N` becomes `aN`, `-beta.N` `bN` and `-rc.N`
/// `rcN`. Any other version is returned as it is.
pub fn pep440(semver: &str) -> String {
    let Some((release, pre)) = semver.split_once('-') else {
        return semver.to_string();
    };
    let Some((kind, n)) = pre.split_once('.') else {
        return semver.to_string();
    };
    let tag = match kind {
        "alpha" => "a",
        "beta" => "b",
        "rc" => "rc",
        _ => return semver.to_string(),
    };
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return semver.to_string();
    }
    format!("{release}{tag}{n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_releases_take_the_pep440_spelling() {
        assert_eq!(pep440("0.2.0-alpha.0"), "0.2.0a0");
        assert_eq!(pep440("1.4.2-beta.3"), "1.4.2b3");
        assert_eq!(pep440("2.0.0-rc.12"), "2.0.0rc12");
        assert_eq!(pep440("0.2.0"), "0.2.0");
    }

    #[test]
    fn other_suffixes_are_kept() {
        assert_eq!(pep440("0.2.0-dev"), "0.2.0-dev");
        assert_eq!(pep440("0.2.0-alpha.x"), "0.2.0-alpha.x");
    }

    #[test]
    fn the_crate_version_is_converted() {
        assert_eq!(*VERSION, pep440(env!("CARGO_PKG_VERSION")));
    }
}
