//! `System.Version`-shaped version numbers, MeloongCore's generic
//! `ValueRange<T>`, VB's `Val`, and PCLCS `McUtils.CompareVersion`.
//!
//! Sources: `MeloongCore/Shared/General/ValueRange.cs`,
//! `MeloongCore/Shared/Utils/StringUtils.cs` (`ParseVersionWithDefaults`),
//! `PCLCS/Minecraft/MCUtils.cs`.

use std::cmp::Ordering;
use std::fmt;

// ---------------------------------------------------------------------------
// JavaVersion — a .NET System.Version (major, minor, build, revision).
// Components not supplied to the .NET constructor are -1 and compare
// *below* an explicit 0, exactly as in .NET.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JavaVersion {
    pub major: i64,
    pub minor: i64,
    pub build: i64,
    pub revision: i64,
}

impl JavaVersion {
    /// `new Version(major, minor)` — build/revision undefined (-1).
    pub const fn dotnet2(major: i64, minor: i64) -> Self {
        Self { major, minor, build: -1, revision: -1 }
    }
    /// `new Version(major, minor, build)` — revision undefined (-1).
    pub const fn dotnet3(major: i64, minor: i64, build: i64) -> Self {
        Self { major, minor, build, revision: -1 }
    }
    /// `new Version(major, minor, build, revision)`.
    pub const fn dotnet4(major: i64, minor: i64, build: i64, revision: i64) -> Self {
        Self { major, minor, build, revision }
    }
    /// The comparable vanilla triple from `version.rs`, as the 3-part
    /// .NET Version PCL builds with `NameToVersion`.
    pub fn from_comparable(v: crate::version::ComparableVersion) -> Self {
        Self::dotnet3(v.major as i64, v.minor as i64, v.patch as i64)
    }
}

impl Ord for JavaVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.build, self.revision)
            .cmp(&(other.major, other.minor, other.build, other.revision))
    }
}
impl PartialOrd for JavaVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for JavaVersion {
    /// .NET `Version.ToString()`: only the components that were defined.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.build < 0 {
            write!(f, "{}.{}", self.major, self.minor)
        } else if self.revision < 0 {
            write!(f, "{}.{}.{}", self.major, self.minor, self.build)
        } else {
            write!(
                f,
                "{}.{}.{}.{}",
                self.major, self.minor, self.build, self.revision
            )
        }
    }
}

/// VB's `Val`: parse a leading numeric prefix (optional sign, digits and
/// one decimal point); no numeric prefix means 0. Hex `&H` / octal `&O`
/// prefixes are also part of `Val` and are honoured. Spaces inside the
/// digit run are ignored by real `Val`; PCL's inputs never rely on that,
/// so the port keeps it simple and stops at the first space.
pub fn vb_val(s: &str) -> f64 {
    let t = s.trim_start();
    if t.len() >= 2 && t[..2].eq_ignore_ascii_case("&h") {
        let hex: String = t[2..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        return i64::from_str_radix(&hex, 16).unwrap_or(0) as f64;
    }
    if t.len() >= 2 && t[..2].eq_ignore_ascii_case("&o") {
        let oct: String = t[2..].chars().take_while(|c| ('0'..='7').contains(c)).collect();
        return i64::from_str_radix(&oct, 8).unwrap_or(0) as f64;
    }
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_digit = false;
    for (i, c) in t.char_indices() {
        match c {
            '+' | '-' if i == 0 => end = i + 1,
            '0'..='9' => {
                seen_digit = true;
                end = i + 1;
            }
            '.' if !seen_dot => {
                seen_dot = true;
                end = i + 1;
            }
            _ => break,
        }
    }
    if !seen_digit {
        return 0.0;
    }
    t[..end].parse::<f64>().unwrap_or(0.0)
}

/// `Val` narrowed to an integer the way VB narrows a Double into a
/// `Version` component (round-half-to-even, like `CInt`).
pub fn vb_val_int(s: &str) -> i64 {
    let v = vb_val(s);
    let floor = v.floor();
    let frac = v - floor;
    let rounded = if frac > 0.5 {
        floor + 1.0
    } else if frac < 0.5 {
        floor
    } else if (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    };
    rounded as i64
}

/// `StringUtils.ParseVersionWithDefaults`: missing segments default to
/// 0 (unlike raw `Version.Parse`, which leaves them at -1), e.g. "2"
/// becomes 2.0.0.0. Extra segments or non-numeric input are errors,
/// mirroring `Version.Parse` throwing.
pub fn parse_version_with_defaults(value: &str) -> Result<JavaVersion, String> {
    let mut v = value.to_string();
    while v.matches('.').count() < 3 {
        v.push_str(".0");
    }
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 4 {
        return Err(format!("version has too many segments: {value}"));
    }
    let mut nums = [0i64; 4];
    for (i, p) in parts.iter().enumerate() {
        nums[i] = p
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("invalid version segment {p:?} in {value:?}"))?;
        if nums[i] < 0 {
            return Err(format!("negative version segment in {value:?}"));
        }
    }
    Ok(JavaVersion::dotnet4(nums[0], nums[1], nums[2], nums[3]))
}

// ---------------------------------------------------------------------------
// ValueRange — port of MeloongCore ValueRange<T> specialized to
// JavaVersion (the only instantiation PCL's Java logic uses).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueRange {
    pub lower: Option<JavaVersion>,
    pub upper: Option<JavaVersion>,
    pub is_lower_inclusive: bool,
    pub is_upper_inclusive: bool,
}

impl ValueRange {
    pub fn closed(lower: JavaVersion, upper: JavaVersion) -> Self {
        Self { lower: Some(lower), upper: Some(upper), is_lower_inclusive: true, is_upper_inclusive: true }
    }
    pub fn closed_open(lower: JavaVersion, upper: JavaVersion) -> Self {
        Self { lower: Some(lower), upper: Some(upper), is_lower_inclusive: true, is_upper_inclusive: false }
    }
    pub fn open_closed(lower: JavaVersion, upper: JavaVersion) -> Self {
        Self { lower: Some(lower), upper: Some(upper), is_lower_inclusive: false, is_upper_inclusive: true }
    }
    pub fn open(lower: JavaVersion, upper: JavaVersion) -> Self {
        Self { lower: Some(lower), upper: Some(upper), is_lower_inclusive: false, is_upper_inclusive: false }
    }
    pub fn exactly(value: JavaVersion) -> Self {
        Self::closed(value, value)
    }
    pub fn at_least(lower: JavaVersion) -> Self {
        Self { lower: Some(lower), upper: None, is_lower_inclusive: true, is_upper_inclusive: true }
    }
    pub fn greater_than(lower: JavaVersion) -> Self {
        Self { lower: Some(lower), upper: None, is_lower_inclusive: false, is_upper_inclusive: true }
    }
    pub fn at_most(upper: JavaVersion) -> Self {
        Self { lower: None, upper: Some(upper), is_lower_inclusive: true, is_upper_inclusive: true }
    }
    pub fn less_than(upper: JavaVersion) -> Self {
        Self { lower: None, upper: Some(upper), is_lower_inclusive: true, is_upper_inclusive: false }
    }
    pub fn all() -> Self {
        Self { lower: None, upper: None, is_lower_inclusive: true, is_upper_inclusive: true }
    }

    pub fn has_lower(&self) -> bool {
        self.lower.is_some()
    }
    pub fn has_upper(&self) -> bool {
        self.upper.is_some()
    }

    pub fn contains(&self, value: JavaVersion) -> bool {
        if let Some(lower) = self.lower {
            let c = value.cmp(&lower);
            if c == Ordering::Less {
                return false;
            }
            if c == Ordering::Equal && !self.is_lower_inclusive {
                return false;
            }
        }
        if let Some(upper) = self.upper {
            let c = value.cmp(&upper);
            if c == Ordering::Greater {
                return false;
            }
            if c == Ordering::Equal && !self.is_upper_inclusive {
                return false;
            }
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        match (self.lower, self.upper) {
            (Some(lower), Some(upper)) => {
                let c = lower.cmp(&upper);
                c == Ordering::Greater
                    || (c == Ordering::Equal && (!self.is_lower_inclusive || !self.is_upper_inclusive))
            }
            _ => false,
        }
    }

    /// MeloongCore `Intersect`: always returns a range (possibly empty),
    /// never null — PCL's caller checks `IsEmpty` afterwards.
    pub fn intersect(&self, other: &ValueRange) -> ValueRange {
        let (lower, is_lower_inclusive) = match (self.lower, other.lower) {
            (Some(a), Some(b)) => match a.cmp(&b) {
                Ordering::Greater => (Some(a), self.is_lower_inclusive),
                Ordering::Less => (Some(b), other.is_lower_inclusive),
                Ordering::Equal => (Some(a), self.is_lower_inclusive && other.is_lower_inclusive),
            },
            (Some(a), None) => (Some(a), self.is_lower_inclusive),
            (None, Some(b)) => (Some(b), other.is_lower_inclusive),
            (None, None) => (None, true),
        };
        let (upper, is_upper_inclusive) = match (self.upper, other.upper) {
            (Some(a), Some(b)) => match a.cmp(&b) {
                Ordering::Less => (Some(a), self.is_upper_inclusive),
                Ordering::Greater => (Some(b), other.is_upper_inclusive),
                Ordering::Equal => (Some(a), self.is_upper_inclusive && other.is_upper_inclusive),
            },
            (Some(a), None) => (Some(a), self.is_upper_inclusive),
            (None, Some(b)) => (Some(b), other.is_upper_inclusive),
            (None, None) => (None, true),
        };
        ValueRange { lower, upper, is_lower_inclusive, is_upper_inclusive }
    }

    /// MeloongCore `FromString` with `ParseVersionWithDefaults` as the
    /// element parser (the combination PCL uses for its Java range
    /// setting): `[low, high]`, `(low, )`, `[, high)`, `-∞` / `+∞` /
    /// empty for open ends; both `,` and `，` separate.
    pub fn from_string(s: &str) -> Result<ValueRange, String> {
        if s.is_empty() {
            return Err("range string is empty".into());
        }
        let chars: Vec<char> = s.chars().collect();
        let is_lower_inclusive = chars[0] == '[';
        if !is_lower_inclusive && chars[0] != '(' {
            return Err("range must start with [ or (".into());
        }
        let is_upper_inclusive = chars[chars.len() - 1] == ']';
        if !is_upper_inclusive && chars[chars.len() - 1] != ')' {
            return Err("range must end with ] or )".into());
        }
        let comma = s
            .char_indices()
            .find(|(_, c)| *c == ',' || *c == '，')
            .map(|(i, _)| i)
            .ok_or("range needs a comma separator")?;
        let lower_str = s[1..comma].trim();
        let upper_str = s[comma + 1..s.len() - 1].trim();
        let lower = if lower_str.is_empty() || lower_str == "-∞" {
            None
        } else {
            Some(parse_version_with_defaults(lower_str)?)
        };
        let upper = if upper_str.is_empty() || upper_str == "+∞" || upper_str == "∞" {
            None
        } else {
            Some(parse_version_with_defaults(upper_str)?)
        };
        Ok(ValueRange { lower, upper, is_lower_inclusive, is_upper_inclusive })
    }
}

impl fmt::Display for ValueRange {
    /// MeloongCore `ToString`, including its quirk: the opening bracket
    /// is `[` only when a lower bound exists *and* is inclusive (so an
    /// open lower bound prints `(` even when inclusive-but-absent), and
    /// symmetrically for the closing bracket.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let open = if self.lower.is_some() && self.is_lower_inclusive { "[" } else { "(" };
        let close = if self.upper.is_some() && self.is_upper_inclusive { "]" } else { ")" };
        let lower = self.lower.map(|v| v.to_string()).unwrap_or_else(|| "-∞".into());
        let upper = self.upper.map(|v| v.to_string()).unwrap_or_else(|| "+∞".into());
        write!(f, "{open}{lower}, {upper}{close}")
    }
}

// ---------------------------------------------------------------------------
// McUtils.CompareVersion — PCLCS string version comparison.
// ---------------------------------------------------------------------------

/// Split into runs of ASCII letters / digits the way the original's
/// `RegexSearch("[a-z]+|[0-9]+")` does (after lowercasing); every other
/// character is a separator and produces no token.
fn version_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_is_digit: Option<bool> = None;
    for c in s.chars() {
        let kind = if c.is_ascii_digit() {
            Some(true)
        } else if c.is_ascii_alphabetic() {
            Some(false)
        } else {
            None
        };
        match kind {
            Some(k) if cur_is_digit == Some(k) => cur.push(c),
            Some(k) => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push(c);
                cur_is_digit = Some(k);
            }
            None => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    cur_is_digit = None;
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn token_number(token: &str) -> i64 {
    match token {
        "rc" => -1,
        "pre" => -2,
        "snapshot" => -3,
        "experimental" => -4,
        _ => token.parse::<i64>().unwrap_or(0),
    }
}

/// Faithful port of PCLCS `McUtils.CompareVersion`: positive when `left`
/// is newer, negative when `right` is newer, 0 when equal. The literal
/// `未知版本` ("unknown version") sorts above everything. C#'s ordinal
/// string comparison is approximated by byte order (identical for the
/// ASCII tokens this function produces).
pub fn compare_version(left: &str, right: &str) -> i32 {
    if left == right {
        return 0;
    }
    if left == "未知版本" || right == "未知版本" {
        return if left == "未知版本" { 1 } else { -1 };
    }
    let left_l = left.to_lowercase();
    let right_l = right.to_lowercase();
    let left_parts = version_tokens(&left_l.replace("快照", "snapshot").replace("预览版", "pre"));
    let right_parts = version_tokens(&right_l.replace("快照", "snapshot").replace("预览版", "pre"));
    let mut i = 0usize;
    loop {
        if i >= left_parts.len() && i >= right_parts.len() {
            return sign_of(left_l.cmp(&right_l)); // ordinal string compare
        }
        let left_value = left_parts.get(i).map(|s| s.as_str()).unwrap_or("0");
        let right_value = right_parts.get(i).map(|s| s.as_str()).unwrap_or("0");
        if left_value == right_value {
            i += 1;
            continue;
        }
        let left_number = token_number(left_value);
        let right_number = token_number(right_value);
        if left_number == 0 && right_number == 0 {
            return sign_of(left_value.cmp(right_value));
        }
        if left_number != right_number {
            return sign_of(left_number.cmp(&right_number));
        }
        i += 1;
    }
}

fn sign_of(o: Ordering) -> i32 {
    match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// `McUtils.CompareVersionGE`.
pub fn compare_version_ge(left: &str, right: &str) -> bool {
    compare_version(left, right) >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_version_ordering_and_display() {
        // Undefined components (-1) sort below explicit 0, as in .NET.
        assert!(JavaVersion::dotnet2(8, 0) < JavaVersion::dotnet3(8, 0, 0));
        assert!(JavaVersion::dotnet3(8, 0, 320) < JavaVersion::dotnet4(8, 0, 320, 0));
        assert_eq!(JavaVersion::dotnet2(21, 0).to_string(), "21.0");
        assert_eq!(JavaVersion::dotnet3(8, 0, 320).to_string(), "8.0.320");
        assert_eq!(JavaVersion::dotnet4(17, 0, 8, 1).to_string(), "17.0.8.1");
    }

    #[test]
    fn val_parsing() {
        assert_eq!(vb_val("21"), 21.0);
        assert_eq!(vb_val("8u51"), 8.0);
        assert_eq!(vb_val("abc"), 0.0);
        assert_eq!(vb_val("  12.5x"), 12.5);
        assert_eq!(vb_val_int("36.2"), 36);
    }

    #[test]
    fn parse_with_defaults_pads() {
        assert_eq!(parse_version_with_defaults("2").unwrap(), JavaVersion::dotnet4(2, 0, 0, 0));
        assert_eq!(
            parse_version_with_defaults("1.20.1").unwrap(),
            JavaVersion::dotnet4(1, 20, 1, 0)
        );
        assert!(parse_version_with_defaults("1.2.3.4.5").is_err());
        assert!(parse_version_with_defaults("x").is_err());
    }

    #[test]
    fn range_factories_and_contains() {
        let r = ValueRange::closed_open(JavaVersion::dotnet2(8, 0), JavaVersion::dotnet2(9, 0));
        assert!(r.contains(JavaVersion::dotnet4(8, 0, 202, 0)));
        assert!(!r.contains(JavaVersion::dotnet2(9, 0)));
        assert!(!r.contains(JavaVersion::dotnet2(7, 0)));
        assert!(ValueRange::at_least(JavaVersion::dotnet2(21, 0)).contains(JavaVersion::dotnet2(25, 0)));
        assert!(ValueRange::all().contains(JavaVersion::dotnet2(6, 0)));
    }

    #[test]
    fn range_intersect_and_empty() {
        let a = ValueRange::at_least(JavaVersion::dotnet2(17, 0));
        let b = ValueRange::less_than(JavaVersion::dotnet2(9, 0));
        let i = a.intersect(&b);
        assert!(i.is_empty());
        let disjoint = ValueRange::closed_open(JavaVersion::dotnet2(8, 0), JavaVersion::dotnet2(11, 0));
        assert!(a.intersect(&disjoint).is_empty());
        let c = ValueRange::closed_open(JavaVersion::dotnet2(8, 0), JavaVersion::dotnet2(25, 0));
        let d = a.intersect(&c);
        assert!(!d.is_empty());
        assert_eq!(d.lower, Some(JavaVersion::dotnet2(17, 0)));
        assert_eq!(d.upper, Some(JavaVersion::dotnet2(25, 0)));
        assert!(!d.is_upper_inclusive);
    }

    #[test]
    fn range_from_string_roundtrip() {
        let r = ValueRange::from_string("[17.0, 21.0)").unwrap();
        assert_eq!(r.lower, Some(JavaVersion::dotnet4(17, 0, 0, 0)));
        assert_eq!(r.upper, Some(JavaVersion::dotnet4(21, 0, 0, 0)));
        assert!(r.is_lower_inclusive && !r.is_upper_inclusive);
        assert_eq!(r.to_string(), "[17.0.0.0, 21.0.0.0)");
        let open = ValueRange::from_string("(, )").unwrap();
        assert_eq!(open.lower, None);
        assert_eq!(open.upper, None);
        assert!(!open.is_empty() && open.contains(JavaVersion::dotnet2(8, 0)));
        // Unbounded bounds print as infinities upstream (no round-trip).
        assert_eq!(ValueRange::all().to_string(), "(-∞, +∞)");
        assert!(ValueRange::from_string("17,21").is_err());
    }

    #[test]
    fn compare_version_semantics() {
        assert!(compare_version("1.20.1", "1.19.4") > 0);
        assert!(compare_version("47.1.99", "47.2.0") < 0);
        assert_eq!(compare_version("36.2.25", "36.2.25"), 0);
        assert!(compare_version_ge("36.2.25", "34.0.0"));
        assert!(!compare_version_ge("33.0.0", "34.0.0"));
        // Pre-release tokens sort below the plain release.
        assert!(compare_version("1.14.4", "1.14.4-pre2") > 0);
        // "未知版本" sorts above everything.
        assert!(compare_version("未知版本", "99.0") > 0);
        assert!(compare_version("1.0", "未知版本") < 0);
    }
}
