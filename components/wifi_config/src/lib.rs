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

// ============================================================================
// InstalledWifiConf
// ============================================================================

/// Upper bound on the size of an installed `wpa_supplicant.conf` we accept.
pub const MAX_INSTALLED_CONF_BYTES: usize = 64 * 1024;

/// A `wpa_supplicant.conf` read from an installed system, validated but kept
/// verbatim so hand-added lines (e.g. `freq_list`) survive being copied.
///
/// Deliberately not serialisable; `Debug` shows only the [`summary`].
///
/// [`summary`]: InstalledWifiConf::summary
#[derive(Clone, PartialEq, Eq)]
pub struct InstalledWifiConf(String);

/// Non-secret facts about an [`InstalledWifiConf`]. Never contains the psk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledWifiSummary {
    /// SSIDs of all `network` blocks that have one, decoded to text.
    pub ssids: Vec<String>,
    /// Value of the top-level `country=` line, if any.
    pub country: Option<String>,
    /// Whether any line sets `freq_list`.
    pub has_freq_list: bool,
    /// Every value of every `freq_list` line, as written (MHz in a sane
    /// configuration), split on whitespace and commas.
    pub freq_list: Vec<String>,
}

/// What the installed `freq_list` pins the radio to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrequencyPin {
    /// No `freq_list`: the radio scans every band.
    NotPinned,
    /// Every listed frequency is a number of at least 5000 MHz.
    FiveGhz,
    /// A `freq_list` that is empty, holds 2.4 GHz values, or anything not a
    /// plain number.
    Other,
}

impl InstalledWifiSummary {
    /// Classify the `freq_list`. 5 GHz only when there is at least one value
    /// and every value is a number of at least 5000.
    pub fn frequency_pin(&self) -> FrequencyPin {
        if !self.has_freq_list {
            return FrequencyPin::NotPinned;
        }
        let all_five_ghz = !self.freq_list.is_empty()
            && self
                .freq_list
                .iter()
                .all(|f| f.parse::<u32>().is_ok_and(|mhz| mhz >= 5000));
        if all_five_ghz {
            FrequencyPin::FiveGhz
        } else {
            FrequencyPin::Other
        }
    }
}

/// One parsed `network={ ... }` block (secret fields reduced to presence).
#[derive(Default)]
struct NetworkBlock {
    ssid: Option<String>,
    has_psk: bool,
    open: bool,
}

impl NetworkBlock {
    fn is_usable(&self) -> bool {
        self.ssid.is_some() && (self.has_psk || self.open)
    }
}

/// Meaningful (non-blank, non-comment) lines, trimmed.
fn conf_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let (k, v) = line.split_once('=')?;
    Some((k.trim(), v.trim()))
}

fn opens_network_block(line: &str) -> bool {
    line.split_once('=')
        .is_some_and(|(k, v)| k.trim() == "network" && v.trim() == "{")
}

fn decode_ssid(value: &str) -> Option<String> {
    if let Some(inner) = value.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        return (!inner.is_empty()).then(|| inner.to_owned());
    }
    let bytes = from_hex(value).filter(|b| !b.is_empty())?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Returns the network blocks, ignoring everything outside them.
fn network_blocks(text: &str) -> Vec<NetworkBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<NetworkBlock> = None;
    for line in conf_lines(text) {
        match current.as_mut() {
            None if opens_network_block(line) => current = Some(NetworkBlock::default()),
            None => {}
            Some(_) if line == "}" => blocks.extend(current.take()),
            Some(block) => match split_key_value(line) {
                Some(("ssid", v)) => block.ssid = decode_ssid(v),
                Some(("psk", v)) => block.has_psk = !v.is_empty(),
                Some(("key_mgmt", "NONE")) => block.open = true,
                _ => {}
            },
        }
    }
    blocks
}

impl InstalledWifiConf {
    /// Accepts the text only if it is at most 64 KiB and has at least one
    /// `network` block with an `ssid` and either a `psk` or `key_mgmt=NONE`.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_INSTALLED_CONF_BYTES {
            return None;
        }
        network_blocks(text)
            .iter()
            .any(NetworkBlock::is_usable)
            .then(|| Self(text.to_owned()))
    }

    /// The verbatim file text, for writing out.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Non-secret summary of the configuration.
    pub fn summary(&self) -> InstalledWifiSummary {
        let ssids = network_blocks(&self.0)
            .into_iter()
            .filter_map(|b| b.ssid)
            .collect();

        let mut country = None;
        let mut has_freq_list = false;
        let mut freq_list = Vec::new();
        let mut depth_in_block = false;
        for line in conf_lines(&self.0) {
            if depth_in_block {
                depth_in_block = line != "}";
            } else if opens_network_block(line) {
                depth_in_block = true;
            }
            match split_key_value(line) {
                Some(("freq_list", v)) => {
                    has_freq_list = true;
                    freq_list.extend(
                        v.split(|c: char| c.is_whitespace() || c == ',')
                            .filter(|t| !t.is_empty())
                            .map(str::to_owned),
                    );
                }
                Some(("country", v)) if !depth_in_block && country.is_none() => {
                    country = Some(v.to_owned());
                }
                _ => {}
            }
        }

        InstalledWifiSummary {
            ssids,
            country,
            has_freq_list,
            freq_list,
        }
    }
}

impl fmt::Debug for InstalledWifiConf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstalledWifiConf")
            .field("summary", &self.summary())
            .field("contents", &format_args!("<redacted>"))
            .finish()
    }
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

    fn quoted_conf(psk_line: &str) -> String {
        format!("country=SE\nnetwork={{\n\tssid=\"Home Net\"\n\t{psk_line}\n}}\n")
    }

    #[test]
    fn rendered_conf_parses_with_matching_ssid() {
        let cfg = NetworkConfig::new(
            WifiSsid::new("IEEE").unwrap(),
            &WifiPassphrase::new("password").unwrap(),
            CountryCode::new("se").unwrap(),
        );
        let rendered = render_wpa_supplicant_conf(&cfg);
        let conf = InstalledWifiConf::parse(&rendered).unwrap();
        let summary = conf.summary();
        assert_eq!(summary.ssids, vec!["IEEE".to_owned()]);
        assert_eq!(summary.country.as_deref(), Some("SE"));
        assert!(!summary.has_freq_list);
        assert_eq!(conf.as_str(), rendered);
    }

    #[test]
    fn quoted_ssid_parses() {
        let conf = InstalledWifiConf::parse(&quoted_conf("psk=\"secret pass\"")).unwrap();
        assert_eq!(conf.summary().ssids, vec!["Home Net".to_owned()]);
    }

    #[test]
    fn open_network_parses() {
        assert!(InstalledWifiConf::parse(&quoted_conf("key_mgmt=NONE")).is_some());
    }

    #[test]
    fn freq_list_preserved_byte_for_byte() {
        let text = "# my wifi\r\n\
            network={\n\
            \tssid=\"Home\"\n\
            \tpsk=\"hunter2hunter2\"\n\
            \x20   freq_list=5180 5200\n\
            \tkey_mgmt=WPA-PSK\n\
            }\n\n";
        let conf = InstalledWifiConf::parse(text).unwrap();
        assert_eq!(conf.as_str(), text);
        assert!(conf.summary().has_freq_list);
    }

    fn pin_of(freq_line: &str) -> FrequencyPin {
        let text =
            format!("network={{\n\tssid=\"Home\"\n\tpsk=\"hunter2hunter2\"\n{freq_line}\n}}\n");
        InstalledWifiConf::parse(&text)
            .unwrap()
            .summary()
            .frequency_pin()
    }

    #[rstest]
    #[case::none("", FrequencyPin::NotPinned)]
    #[case::five_ghz("\tfreq_list=5180 5200", FrequencyPin::FiveGhz)]
    #[case::five_ghz_commas("\tfreq_list=5180,5745", FrequencyPin::FiveGhz)]
    #[case::boundary("\tfreq_list=5000", FrequencyPin::FiveGhz)]
    #[case::two_four("\tfreq_list=2412 2437", FrequencyPin::Other)]
    #[case::mixed("\tfreq_list=2412 5180", FrequencyPin::Other)]
    #[case::just_below("\tfreq_list=4999", FrequencyPin::Other)]
    #[case::empty_value("\tfreq_list=", FrequencyPin::Other)]
    #[case::not_a_number("\tfreq_list=5180 abc", FrequencyPin::Other)]
    #[case::range("\tfreq_list=5180-5320", FrequencyPin::Other)]
    fn frequency_pin_classification(#[case] line: &str, #[case] expected: FrequencyPin) {
        assert_eq!(pin_of(line), expected);
    }

    #[test]
    fn summary_lists_freq_list_values() {
        let text =
            "network={\n\tssid=\"Home\"\n\tpsk=\"hunter2hunter2\"\n\tfreq_list=5180 5200\n}\n";
        let summary = InstalledWifiConf::parse(text).unwrap().summary();
        assert_eq!(
            summary.freq_list,
            vec!["5180".to_owned(), "5200".to_owned()]
        );
    }

    #[test]
    fn tolerates_comments_and_spacing() {
        let text = "  # c\n  network = {\n  # ssid=\"No\"\n  ssid = 4173\n  psk = abc\n  }\n";
        let conf = InstalledWifiConf::parse(text).unwrap();
        assert_eq!(conf.summary().ssids, vec!["As".to_owned()]);
    }

    #[rstest]
    #[case::no_network("country=SE\nctrl_interface=/run/x\n")]
    #[case::no_ssid("network={\n\tpsk=\"abcdefgh\"\n}\n")]
    #[case::no_psk_or_open("network={\n\tssid=\"Home\"\n\tkey_mgmt=WPA-PSK\n}\n")]
    #[case::unterminated("network={\n\tssid=\"Home\"\n\tpsk=\"abcdefgh\"\n")]
    #[case::garbage("\u{0}\u{1}garbage {{{ ===")]
    #[case::empty("")]
    fn rejects_invalid(#[case] text: &str) {
        assert!(InstalledWifiConf::parse(text).is_none());
    }

    #[test]
    fn rejects_oversized() {
        let ok = quoted_conf("psk=\"abcdefgh\"");
        let padding = "#".repeat(MAX_INSTALLED_CONF_BYTES);
        assert!(InstalledWifiConf::parse(&ok).is_some());
        assert!(InstalledWifiConf::parse(&format!("{ok}{padding}")).is_none());
    }

    #[test]
    fn accepts_exactly_max_size() {
        let ok = quoted_conf("psk=\"abcdefgh\"");
        let padding = "#".repeat(MAX_INSTALLED_CONF_BYTES - ok.len());
        let text = format!("{ok}{padding}");
        assert_eq!(text.len(), MAX_INSTALLED_CONF_BYTES);
        assert!(InstalledWifiConf::parse(&text).is_some());
    }

    proptest! {
        #[test]
        fn installed_conf_never_leaks_psk(psk in "[!#-~]{8,63}") {
            let baseline = InstalledWifiConf::parse(&quoted_conf("psk=\"x\"")).unwrap();
            let baseline_out = format!("{baseline:?}{:?}", baseline.summary());
            prop_assume!(!baseline_out.contains(&psk));

            let conf = InstalledWifiConf::parse(&quoted_conf(&format!("psk=\"{psk}\""))).unwrap();
            let debug = format!("{conf:?}");
            let summary = format!("{:?}", conf.summary());
            prop_assert!(!debug.contains(&psk));
            prop_assert!(!summary.contains(&psk));
        }

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
