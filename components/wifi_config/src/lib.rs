//! Pure wifi provisioning types and `wpa_supplicant` config rendering.
//!
//! Secrets are handled defensively: the user's passphrase is never stored
//! after derivation, and neither the passphrase nor the derived key can leak
//! through `Debug`. The SSID is always rendered as unquoted hex so no
//! escaping is ever required.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Path (relative to a root mount) of the file the stock Void runit
/// `wpa_supplicant` service reads.
pub const WPA_SUPPLICANT_CONF_PATH: &str = "etc/wpa_supplicant/wpa_supplicant.conf";

/// Path (relative to a root mount) of the runit service `conf` file that
/// pins the stock service to an interface.
pub const WPA_SERVICE_CONF_PATH: &str = "etc/sv/wpa_supplicant/conf";

/// The wireless interface the provisioned system uses.
pub const WIFI_INTERFACE: &str = "wlan0";

/// Contents of the runit service `conf` file: pins the service to
/// [`WIFI_INTERFACE`].
pub fn render_wpa_service_conf() -> String {
    format!("WPA_INTERFACE={WIFI_INTERFACE}\n")
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WifiError {
    #[error("SSID must be 1-32 bytes, got {0}")]
    InvalidSsidLength(usize),
    #[error("SSID must not contain control characters")]
    InvalidSsidChars,
    #[error("passphrase must be 8-63 printable ASCII characters or 64 hex characters")]
    InvalidPassphrase,
    #[error("country code must be exactly two ASCII letters")]
    InvalidCountryCode,
    #[error("PSK must be 64 hex characters")]
    InvalidPsk,
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Decodes a string of hex characters. Returns `None` unless the input is
/// even-length and hex-only.
fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

// ============================================================================
// WifiSsid
// ============================================================================

/// Network name: 1-32 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiSsid(String);

impl WifiSsid {
    pub fn new(s: &str) -> Result<Self, WifiError> {
        match s.len() {
            1..=32 if s.bytes().any(|b| b.is_ascii_control()) => Err(WifiError::InvalidSsidChars),
            1..=32 => Ok(Self(s.to_owned())),
            n => Err(WifiError::InvalidSsidLength(n)),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Lowercase hex of the SSID bytes.
    pub fn to_hex(&self) -> String {
        to_hex(self.0.as_bytes())
    }
}

impl fmt::Display for WifiSsid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for WifiSsid {
    type Err = WifiError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for WifiSsid {
    type Error = WifiError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

// ============================================================================
// WifiPassphrase
// ============================================================================

/// User-entered passphrase: 8-63 printable ASCII or exactly 64 hex characters.
/// Never printed, serialised or cloned.
pub struct WifiPassphrase(Inner);

/// Passphrase kind, decided once at construction.
enum Inner {
    Passphrase(String),
    RawKey([u8; 32]),
}

impl WifiPassphrase {
    pub fn new(s: &str) -> Result<Self, WifiError> {
        match s.len() {
            8..=63 if s.bytes().all(|b| (0x20..=0x7e).contains(&b)) => {
                Ok(Self(Inner::Passphrase(s.to_owned())))
            }
            64 => {
                let bytes = from_hex(s).ok_or(WifiError::InvalidPassphrase)?;
                let key = <[u8; 32]>::try_from(bytes).map_err(|_| WifiError::InvalidPassphrase)?;
                Ok(Self(Inner::RawKey(key)))
            }
            _ => Err(WifiError::InvalidPassphrase),
        }
    }
}

impl fmt::Debug for WifiPassphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WifiPassphrase(<redacted>)")
    }
}

impl FromStr for WifiPassphrase {
    type Err = WifiError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for WifiPassphrase {
    type Error = WifiError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

// ============================================================================
// CountryCode
// ============================================================================

/// ISO 3166 alpha-2 style country code, normalised to uppercase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountryCode(String);

impl CountryCode {
    pub fn new(s: &str) -> Result<Self, WifiError> {
        if s.len() == 2 && s.bytes().all(|b| b.is_ascii_alphabetic()) {
            Ok(Self(s.to_ascii_uppercase()))
        } else {
            Err(WifiError::InvalidCountryCode)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CountryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CountryCode {
    type Err = WifiError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for CountryCode {
    type Error = WifiError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

// ============================================================================
// WpaPsk
// ============================================================================

/// 256-bit WPA pre-shared key derived from passphrase and SSID.
#[derive(Clone, PartialEq, Eq)]
pub struct WpaPsk([u8; 32]);

impl WpaPsk {
    /// PBKDF2-HMAC-SHA1, 4096 iterations, salt = SSID. A 64-hex passphrase is
    /// the key itself and is used directly.
    pub fn derive(passphrase: &WifiPassphrase, ssid: &WifiSsid) -> Self {
        match &passphrase.0 {
            Inner::RawKey(key) => Self(*key),
            Inner::Passphrase(p) => {
                let mut key = [0u8; 32];
                pbkdf2::pbkdf2_hmac::<sha1::Sha1>(p.as_bytes(), ssid.as_bytes(), 4096, &mut key);
                Self(key)
            }
        }
    }

    /// Lowercase hex of the key.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }
}

impl fmt::Debug for WpaPsk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WpaPsk(<redacted>)")
    }
}

// ============================================================================
// NetworkConfig
// ============================================================================

/// Everything needed to join a wifi network. Holds the derived key only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkConfig {
    ssid: WifiSsid,
    psk: WpaPsk,
    country: CountryCode,
}

impl NetworkConfig {
    pub fn new(ssid: WifiSsid, passphrase: &WifiPassphrase, country: CountryCode) -> Self {
        let psk = WpaPsk::derive(passphrase, &ssid);
        Self { ssid, psk, country }
    }

    pub fn ssid(&self) -> &WifiSsid {
        &self.ssid
    }

    pub fn psk(&self) -> &WpaPsk {
        &self.psk
    }

    pub fn country(&self) -> &CountryCode {
        &self.country
    }
}

/// Renders `wpa_supplicant.conf`. The SSID is always unquoted hex.
pub fn render_wpa_supplicant_conf(config: &NetworkConfig) -> String {
    format!(
        "# Generated by MDMA beacon\n\
         ctrl_interface=DIR=/run/wpa_supplicant GROUP=wheel\n\
         update_config=0\n\
         country={country}\n\
         network={{\n\
         \tssid={ssid}\n\
         \tpsk={psk}\n\
         \tkey_mgmt=WPA-PSK\n\
         }}\n",
        country = config.country,
        ssid = config.ssid.to_hex(),
        psk = config.psk.to_hex(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rstest::rstest;

    #[rstest]
    #[case(0, false)]
    #[case(1, true)]
    #[case(32, true)]
    #[case(33, false)]
    fn ssid_length(#[case] len: usize, #[case] ok: bool) {
        assert_eq!(WifiSsid::new(&"a".repeat(len)).is_ok(), ok);
    }

    #[rstest]
    #[case("a".repeat(7), false)]
    #[case("a".repeat(8), true)]
    #[case("a".repeat(63), true)]
    #[case("0123456789abcdefABCDEF0123456789abcdef0123456789abcdef0123456789".to_string(), true)]
    #[case("g".repeat(64), false)]
    #[case("pässwörd1234".to_string(), false)]
    #[case("pass\tword1234".to_string(), false)]
    fn passphrase_validation(#[case] input: String, #[case] ok: bool) {
        assert_eq!(WifiPassphrase::new(&input).is_ok(), ok);
    }

    #[rstest]
    #[case("se", Some("SE"))]
    #[case("Se", Some("SE"))]
    #[case("S", None)]
    #[case("SWE", None)]
    #[case("S1", None)]
    fn country_validation(#[case] input: &str, #[case] expected: Option<&str>) {
        assert_eq!(
            CountryCode::new(input).ok().map(|c| c.to_string()),
            expected.map(str::to_owned)
        );
    }

    fn derive(pass: &str, ssid: &str) -> String {
        WpaPsk::derive(
            &WifiPassphrase::new(pass).unwrap(),
            &WifiSsid::new(ssid).unwrap(),
        )
        .to_hex()
    }

    #[rstest]
    #[case(
        "password",
        "IEEE",
        "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
    )]
    #[case(
        "ThisIsAPassword",
        "ThisIsASSID",
        "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af"
    )]
    fn known_answer(#[case] pass: &str, #[case] ssid: &str, #[case] expected: &str) {
        assert_eq!(derive(pass, ssid), expected);
    }

    #[test]
    fn renders_exact_conf() {
        let cfg = NetworkConfig::new(
            WifiSsid::new("IEEE").unwrap(),
            &WifiPassphrase::new("password").unwrap(),
            CountryCode::new("se").unwrap(),
        );
        let expected = "# Generated by MDMA beacon\n\
ctrl_interface=DIR=/run/wpa_supplicant GROUP=wheel\n\
update_config=0\n\
country=SE\n\
network={\n\
\tssid=49454545\n\
\tpsk=f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e\n\
\tkey_mgmt=WPA-PSK\n\
}\n";
        assert_eq!(render_wpa_supplicant_conf(&cfg), expected);
    }

    #[test]
    fn service_conf_and_paths() {
        assert_eq!(WIFI_INTERFACE, "wlan0");
        assert_eq!(render_wpa_service_conf(), "WPA_INTERFACE=wlan0\n");
        assert_eq!(
            WPA_SUPPLICANT_CONF_PATH,
            "etc/wpa_supplicant/wpa_supplicant.conf"
        );
        assert_eq!(WPA_SERVICE_CONF_PATH, "etc/sv/wpa_supplicant/conf");
    }

    #[rstest]
    #[case("bad\nname")]
    #[case("bad\tname")]
    #[case("bad\x7fname")]
    #[case("bad\0name")]
    fn ssid_rejects_control_chars(#[case] input: &str) {
        assert_eq!(WifiSsid::new(input), Err(WifiError::InvalidSsidChars));
    }

    proptest! {
        #[test]
        fn ssid_hex_round_trips(s in "[^\\x00-\\x1f\\x7f]{1,32}") {
            prop_assume!(s.len() <= 32);
            let ssid = WifiSsid::new(&s).unwrap();
            prop_assert_eq!(from_hex(&ssid.to_hex()).unwrap(), s.as_bytes().to_vec());
        }

        #[test]
        fn ssid_with_control_char_rejected(
            pre in "[a-z]{0,10}",
            c in prop::sample::select((0u8..=0x1f).chain([0x7f]).collect::<Vec<_>>()),
            post in "[a-z]{0,10}",
        ) {
            let s = format!("{pre}{}{post}", c as char);
            prop_assert!(WifiSsid::new(&s).is_err());
        }

        #[test]
        fn rendered_ssid_is_hex_of_bytes(s in "[ -~]{1,32}") {
            let cfg = NetworkConfig::new(
                WifiSsid::new(&s).unwrap(),
                &WifiPassphrase::new("password").unwrap(),
                CountryCode::new("SE").unwrap(),
            );
            let conf = render_wpa_supplicant_conf(&cfg);
            let expected = format!("\tssid={}\n", to_hex(s.as_bytes()));
            prop_assert!(conf.contains(&expected));
        }

        #[test]
        fn printable_passphrase_accepted(p in "[ -~]{8,63}") {
            prop_assert!(WifiPassphrase::new(&p).is_ok());
        }

        #[test]
        fn secrets_never_leak(p in "[ -~]{8,63}", ssid in "net_[0-9]{1,10}") {
            prop_assume!(p.bytes().any(|b| !b.is_ascii_hexdigit()));
            let pass = WifiPassphrase::new(&p).unwrap();
            let cfg = NetworkConfig::new(
                WifiSsid::new(&ssid).unwrap(),
                &pass,
                CountryCode::new("SE").unwrap(),
            );
            let conf = render_wpa_supplicant_conf(&cfg);
            let outputs = [
                format!("{cfg:?}"),
                format!("{pass:?}"),
                format!("{:?}", cfg.psk()),
                conf.clone(),
            ];
            for out in &outputs {
                prop_assert!(!out.contains(&p));
                prop_assert!(!out.contains("psk=\""));
            }
            for out in &outputs[..3] {
                prop_assert!(!out.contains(&cfg.psk().to_hex()));
            }
        }

        #[test]
        fn derivation_is_deterministic(p in "[ -~]{8,63}", ssid in "[ -~]{1,32}") {
            let pass = WifiPassphrase::new(&p).unwrap();
            let ssid = WifiSsid::new(&ssid).unwrap();
            prop_assert_eq!(WpaPsk::derive(&pass, &ssid), WpaPsk::derive(&pass, &ssid));
        }

        #[test]
        fn hex_passphrase_round_trips(h in "[0-9a-fA-F]{64}", ssid in "[ -~]{1,32}") {
            let pass = WifiPassphrase::new(&h).unwrap();
            let ssid = WifiSsid::new(&ssid).unwrap();
            prop_assert_eq!(WpaPsk::derive(&pass, &ssid).to_hex(), h.to_ascii_lowercase());
        }
    }
}
