//! Account data model — the four PCL login types and the flat
//! settings shape PCL persists them in.
//!
//! Sources: `Plain Craft Launcher 2/Modules/Minecraft/ModLaunch.vb`
//! (the `McLoginType` enum, the `McLoginData` class hierarchy,
//! `McLoginResult`, `McLoginName`, the loaders' cache writes), the
//! `GetLoginData` / `IsVaild` functions in `Pages/PageLaunch/
//! PageLogin{Ms,Legacy,Nide,Auth}.xaml.vb` and their `*Skin` variants,
//! and the setting entries in `Pages/PageSetup/Settings.vb`.
//!
//! Scope: data structures and (de)serialization only. No login network
//! flow (OAuth, Yggdrasil validate/refresh/authenticate) is ported
//! here — that is M2 work; this module models what those flows read
//! and write.
//!
//! Cross-platform: the model is deliberately platform-neutral. PCL
//! itself stores these values in the Windows registry (most account
//! keys flagged `Encrypted`); the port models the same flat key/value
//! *shape* (see [`AccountStore`], whose serde field names are the
//! setting keys), and whichever persistence backend a platform uses —
//! registry on Windows, a file or keychain elsewhere — lives in the
//! platform layer, not here. Token/password fields are plain strings
//! in memory, exactly as in PCL's classes; encrypting them at rest is
//! the backend's job, as it is PCL's settings layer's job there.
//!
//! Test fixtures in this module use obviously fake credential values.

use crate::pathutil::before_first;
use crate::version::ComparableVersion;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---------------------------------------------------------------------------
// McLoginType (ModLaunch.vb).
// ---------------------------------------------------------------------------

/// PCL's login-type enum. The numeric values are the original's
/// (`Legacy = 0, Nide = 2, Auth = 3, Ms = 5` — note the gaps) and are
/// also its serde representation here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McLoginType {
    #[default]
    Legacy = 0,
    Nide = 2,
    Auth = 3,
    Ms = 5,
}

impl McLoginType {
    pub fn from_value(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::Legacy),
            2 => Some(Self::Nide),
            3 => Some(Self::Auth),
            5 => Some(Self::Ms),
            _ => None,
        }
    }

    pub fn value(self) -> i64 {
        self as i64
    }

    /// The VB enum member name (`McLoginType.ToString`), which PCL also
    /// uses as the `LoginType` launch-argument value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Legacy => "Legacy",
            Self::Nide => "Nide",
            Self::Auth => "Auth",
            Self::Ms => "Ms",
        }
    }
}

impl Serialize for McLoginType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i64(self.value())
    }
}

impl<'de> Deserialize<'de> for McLoginType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i64::deserialize(deserializer)?;
        Self::from_value(value)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown McLoginType value {value}")))
    }
}

// ---------------------------------------------------------------------------
// Stable hash + identity (MeloongCore StringExtensions.GetStableHashCode,
// ModLaunch.vb GetHashCode overrides).
// ---------------------------------------------------------------------------

/// MeloongCore `StringUtils.GetStableHashCode`: a djb-style hash over
/// UTF-16 code units, stable across environments. Ported exactly
/// (wrapping u64 arithmetic, as in the unchecked C# original).
pub fn stable_hash_code(text: &str) -> u64 {
    let mut result: u64 = 5381;
    for unit in text.encode_utf16() {
        result = (result << 5) ^ result ^ u64::from(unit);
    }
    result ^ 0xA98F_501B_C684_032F
}

/// PCL's per-class `GetHashCode`: the stable hash of the class's
/// identity string, reduced `Mod Integer.MaxValue`.
fn login_hash_code(identity: &str) -> i32 {
    (stable_hash_code(identity) % 2_147_483_647) as i32
}

// ---------------------------------------------------------------------------
// The McLoginData hierarchy (ModLaunch.vb).
//
// VB String fields without an initializer are Nothing in the original;
// PCL's own code consistently treats them like "" (VB `&` concatenation
// and `= ""` comparisons conflate the two), so the port normalizes
// them to String with a "" default and says so here once.
// ---------------------------------------------------------------------------

/// Microsoft login input (`McLoginMs`). All fields default to "" as in
/// the original. Only `oauth_refresh_token` + `user_name` take part in
/// the identity hash; `access_token` / `uuid` / `profile_json` are
/// filled in once a cached session exists (the `PageLoginMsSkin`
/// form of the data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McLoginMs {
    #[serde(rename = "Type")]
    pub login_type: McLoginType,
    /// Cached OAuth refresh token; empty when never logged in.
    #[serde(rename = "OAuthRefreshToken", default)]
    pub oauth_refresh_token: String,
    #[serde(rename = "AccessToken", default)]
    pub access_token: String,
    #[serde(rename = "Uuid", default)]
    pub uuid: String,
    #[serde(rename = "UserName", default)]
    pub user_name: String,
    /// The Microsoft profile JSON, cached verbatim as a string.
    #[serde(rename = "ProfileJson", default)]
    pub profile_json: String,
}

impl Default for McLoginMs {
    fn default() -> Self {
        Self {
            login_type: McLoginType::Ms,
            oauth_refresh_token: String::new(),
            access_token: String::new(),
            uuid: String::new(),
            user_name: String::new(),
            profile_json: String::new(),
        }
    }
}

impl McLoginMs {
    /// `PageLoginMs.IsVaild`: a refresh token must exist.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.oauth_refresh_token.is_empty() {
            Err("请在登录账号后再启动游戏！")
        } else {
            Ok(())
        }
    }

    /// The original's hash input: `OAuthRefreshToken & UserName`.
    pub fn identity_string(&self) -> String {
        format!("{}{}", self.oauth_refresh_token, self.user_name)
    }

    pub fn hash_code(&self) -> i32 {
        login_hash_code(&self.identity_string())
    }
}

/// Offline ("Legacy") login input (`McLoginLegacy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McLoginLegacy {
    #[serde(rename = "Type")]
    pub login_type: McLoginType,
    #[serde(rename = "UserName", default)]
    pub user_name: String,
    /// Offline skin kind, from the `LaunchSkinType` setting:
    /// 0 = default, 1 = force Steve, 2 = force Alex,
    /// 3 = use a genuine player's skin (`skin_name` is that player),
    /// 4 = custom (slim decided by the `LaunchSkinSlim` setting).
    #[serde(rename = "SkinType", default)]
    pub skin_type: i32,
    /// The genuine player name when `skin_type == 3`.
    #[serde(rename = "SkinName", default)]
    pub skin_name: String,
}

impl Default for McLoginLegacy {
    fn default() -> Self {
        Self {
            login_type: McLoginType::Legacy,
            user_name: String::new(),
            skin_type: 0,
            skin_name: String::new(),
        }
    }
}

/// The 1.20.3 comparable version, `New Version(20, 0, 3)` in the
/// original's name-length check.
const VANILLA_1_20_3: ComparableVersion = ComparableVersion {
    major: 20,
    minor: 0,
    patch: 3,
};

impl McLoginLegacy {
    /// `PageLoginLegacy.IsVaild`, minus the version-dependent check.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.validate_for_version(None)
    }

    /// `PageLoginLegacy.IsVaild`. Pass the selected instance's vanilla
    /// version to enable the 1.20.3+ name-length rule (the original
    /// applies it only when an instance is selected and its version is
    /// valid). Name length is measured in UTF-16 code units after
    /// trimming, as VB's `String.Length` does.
    pub fn validate_for_version(
        &self,
        vanilla: Option<ComparableVersion>,
    ) -> Result<(), &'static str> {
        if self.user_name.trim().is_empty() {
            return Err("玩家名不能为空！");
        }
        if self.user_name.contains('"') {
            return Err("玩家名不能包含英文引号！");
        }
        if let Some(vanilla) = vanilla {
            if vanilla.is_valid()
                && vanilla >= VANILLA_1_20_3
                && self.user_name.trim().encode_utf16().count() > 16
            {
                return Err("自 1.20.3 起，玩家名至多只能包含 16 个字符！");
            }
        }
        Ok(())
    }

    /// The original's hash input: `UserName & SkinType & SkinName &
    /// Type` (the enum contributes its member name).
    pub fn identity_string(&self) -> String {
        format!(
            "{}{}{}{}",
            self.user_name,
            self.skin_type,
            self.skin_name,
            self.login_type.name()
        )
    }

    pub fn hash_code(&self) -> i32 {
        login_hash_code(&self.identity_string())
    }
}

/// The fixed Nide (统一通行证) auth-server host. PCL builds Nide base
/// URLs as `https://auth.mc-user.com:233/<server id>/authserver`.
pub const NIDE_AUTH_HOST: &str = "https://auth.mc-user.com:233";

/// The authlib-injector base URL for a server address
/// (`<server>/authserver`, `PageLoginAuth.GetLoginData`).
pub fn auth_base_url(server: &str) -> String {
    format!("{server}/authserver")
}

/// The Nide base URL for a server id (`PageLoginNide.GetLoginData`).
pub fn nide_base_url(server_id: &str) -> String {
    format!("{NIDE_AUTH_HOST}/{server_id}/authserver")
}

/// Server-based login input (`McLoginServer`), shared by the Auth
/// (authlib-injector) and Nide （统一通行证） types; `login_type`
/// distinguishes them, as does the `token` string ("Auth" / "Nide"),
/// which PCL also uses to build settings key names (`Cache<Token>*`,
/// `Login<Token>*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McLoginServer {
    #[serde(rename = "Type")]
    pub login_type: McLoginType,
    #[serde(rename = "UserName", default)]
    pub user_name: String,
    #[serde(rename = "Password", default)]
    pub password: String,
    /// Auth-server base address, including the `/authserver` suffix.
    #[serde(rename = "BaseUrl", default)]
    pub base_url: String,
    /// Identifier string: "Auth" or "Nide".
    #[serde(rename = "Token", default)]
    pub token: String,
    /// Human-readable kind: "Authlib-Injector" or "统一通行证".
    #[serde(rename = "Description", default)]
    pub description: String,
    /// Force re-selecting the player profile on this login
    /// (authlib-injector only). Defaults to false, as in the original.
    #[serde(rename = "ForceReselectProfile", default)]
    pub force_reselect_profile: bool,
}

impl McLoginServer {
    /// The `PageLoginAuth.GetLoginData` shape for a server address
    /// (global setting or the selected instance's override — the
    /// caller supplies whichever applies, as in the original).
    pub fn auth(user_name: &str, password: &str, server: &str) -> Self {
        Self {
            login_type: McLoginType::Auth,
            user_name: user_name.to_string(),
            password: password.to_string(),
            base_url: auth_base_url(server),
            token: "Auth".to_string(),
            description: "Authlib-Injector".to_string(),
            force_reselect_profile: false,
        }
    }

    /// The `PageLoginNide.GetLoginData` shape for a server id.
    pub fn nide(user_name: &str, password: &str, server_id: &str) -> Self {
        Self {
            login_type: McLoginType::Nide,
            user_name: user_name.to_string(),
            password: password.to_string(),
            base_url: nide_base_url(server_id),
            token: "Nide".to_string(),
            description: "统一通行证".to_string(),
            force_reselect_profile: false,
        }
    }

    /// `PageLoginAuth.IsVaild` / `PageLoginNide.IsVaild` (identical).
    /// PCL strips "¨" from the inputs in GetLoginData before this
    /// runs; the check itself only tests emptiness.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.user_name.is_empty() {
            return Err("账号不能为空！");
        }
        if self.password.is_empty() {
            return Err("密码不能为空！");
        }
        Ok(())
    }

    /// The original's hash input:
    /// `UserName & Password & BaseUrl & Token & Type`.
    pub fn identity_string(&self) -> String {
        format!(
            "{}{}{}{}{}",
            self.user_name,
            self.password,
            self.base_url,
            self.token,
            self.login_type.name()
        )
    }

    pub fn hash_code(&self) -> i32 {
        login_hash_code(&self.identity_string())
    }
}

/// The login input, one of the three shapes — the port of the abstract
/// `McLoginData`. Serialization writes the concrete class's fields
/// (including its numeric `Type`); deserialization dispatches on that
/// `Type` value, since the Server shape covers both Nide (2) and
/// Auth (3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McLoginData {
    Ms(McLoginMs),
    Legacy(McLoginLegacy),
    Server(McLoginServer),
}

impl McLoginData {
    pub fn login_type(&self) -> McLoginType {
        match self {
            Self::Ms(data) => data.login_type,
            Self::Legacy(data) => data.login_type,
            Self::Server(data) => data.login_type,
        }
    }

    pub fn identity_string(&self) -> String {
        match self {
            Self::Ms(data) => data.identity_string(),
            Self::Legacy(data) => data.identity_string(),
            Self::Server(data) => data.identity_string(),
        }
    }

    pub fn hash_code(&self) -> i32 {
        login_hash_code(&self.identity_string())
    }

    /// PCL's `McLoginData.Equals`: two login inputs are equal iff
    /// their `GetHashCode()` values are equal — regardless of which
    /// class each side is. (The derived `PartialEq` on this enum is
    /// the stricter structural comparison; this method is the exact
    /// original semantics, hash collisions included.)
    pub fn pcl_equals(&self, other: &Self) -> bool {
        self.hash_code() == other.hash_code()
    }
}

impl Serialize for McLoginData {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Ms(data) => data.serialize(serializer),
            Self::Legacy(data) => data.serialize(serializer),
            Self::Server(data) => data.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for McLoginData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let type_value = value
            .get("Type")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| serde::de::Error::custom("McLoginData: missing numeric \"Type\""))?;
        let login_type = McLoginType::from_value(type_value).ok_or_else(|| {
            serde::de::Error::custom(format!("McLoginData: unknown Type {type_value}"))
        })?;
        fn convert<T: serde::de::DeserializeOwned>(
            v: serde_json::Value,
        ) -> Result<T, serde_json::Error> {
            serde_json::from_value(v)
        }
        match login_type {
            McLoginType::Ms => Ok(Self::Ms(convert(value).map_err(serde::de::Error::custom)?)),
            McLoginType::Legacy => {
                Ok(Self::Legacy(convert(value).map_err(serde::de::Error::custom)?))
            }
            McLoginType::Nide | McLoginType::Auth => {
                Ok(Self::Server(convert(value).map_err(serde::de::Error::custom)?))
            }
        }
    }
}

/// The result of a finished login (`McLoginResult` structure).
/// `result_type` is a display string in the original — "Microsoft"
/// for Ms logins, "Legacy" for offline ones, the auth server's own
/// type string for server logins.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McLoginResult {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Uuid", default)]
    pub uuid: String,
    #[serde(rename = "AccessToken", default)]
    pub access_token: String,
    #[serde(rename = "Type", default)]
    pub result_type: String,
    #[serde(rename = "ClientToken", default)]
    pub client_token: String,
    /// The Microsoft profile JSON (Ms logins only).
    #[serde(rename = "ProfileJson", default)]
    pub profile_json: String,
}

// ---------------------------------------------------------------------------
// List-shaped setting values: LoginMsJson and the ¨-joined histories.
// ---------------------------------------------------------------------------

/// The ¨ (U+00A8) separator PCL uses to pack string lists into single
/// setting values (`LoginLegacyName`, `Login<Token>Email`,
/// `Login<Token>Pass`).
pub const LIST_SEPARATOR: char = '¨';

/// Split a ¨-joined setting value. PCL always guards the empty string
/// before splitting (an empty setting means "no entries"); this
/// helper folds that guard in.
pub fn split_list_setting(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value.split(LIST_SEPARATOR).map(str::to_string).collect()
    }
}

/// Join entries into a ¨-joined setting value ("" when empty).
pub fn join_list_setting(entries: &[String]) -> String {
    entries.join(&LIST_SEPARATOR.to_string())
}

/// The Microsoft account list stored in the `LoginMsJson` setting: a
/// JSON object mapping player name to OAuth refresh token,
/// insertion-ordered like Newtonsoft's JObject (serde_json's
/// `preserve_order` map keeps the same order when parsing).
///
/// PCL behaviors mirrored here: a successful login removes the
/// *input's* old name (the player may have been renamed) and sets the
/// result name's token; deleting an account removes its key; a parse
/// failure makes the login page reset the setting to "{}".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MsAccountList {
    entries: Vec<(String, String)>,
}

impl MsAccountList {
    /// Parse the raw `LoginMsJson` setting value.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        let mut entries = Vec::new();
        if let Some(object) = value.as_object() {
            for (name, token) in object {
                // JObject values are tokens; non-string values render
                // as their JSON text, like JToken.ToString does for
                // the combo-box Tag in PageLoginMs.
                let token = match token {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                entries.push((name.clone(), token));
            }
        }
        Ok(Self { entries })
    }

    /// The compact JSON form PCL writes back
    /// (`JObject.ToString(Formatting.None)`).
    pub fn to_json(&self) -> String {
        let mut map = serde_json::Map::new();
        for (name, token) in &self.entries {
            map.insert(name.clone(), serde_json::Value::String(token.clone()));
        }
        serde_json::Value::Object(map).to_string()
    }

    pub fn entries(&self) -> &[(String, String)] {
        &self.entries
    }

    pub fn get(&self, user_name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == user_name)
            .map(|(_, token)| token.as_str())
    }

    /// Remove an account (the delete button in `PageLoginMs`, and the
    /// rename step of a successful login).
    pub fn remove(&mut self, user_name: &str) {
        self.entries.retain(|(name, _)| name != user_name);
    }

    /// `JObject[name] = token`: replaces in place when the name
    /// exists, appends otherwise.
    pub fn set(&mut self, user_name: &str, refresh_token: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|(name, _)| name == user_name) {
            entry.1 = refresh_token.to_string();
        } else {
            self.entries
                .push((user_name.to_string(), refresh_token.to_string()));
        }
    }

    /// The write a successful Microsoft login performs
    /// (`McLoginMsStart`): drop the input's (possibly outdated) name,
    /// then record the result name with the fresh refresh token.
    pub fn record_login(&mut self, input_user_name: &str, result_user_name: &str, refresh_token: &str) {
        self.remove(input_user_name);
        self.set(result_user_name, refresh_token);
    }
}

impl Serialize for MsAccountList {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serde_json::Map::new();
        for (name, token) in &self.entries {
            map.insert(name.clone(), serde_json::Value::String(token.clone()));
        }
        serde_json::Value::Object(map).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MsAccountList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let mut entries = Vec::new();
        if let Some(object) = value.as_object() {
            for (name, token) in object {
                let token = match token {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                entries.push((name.clone(), token));
            }
        }
        Ok(Self { entries })
    }
}

/// A remembered-credentials history for one server login type: the
/// parallel ¨-joined `Login<Token>Email` / `Login<Token>Pass` setting
/// pair, most-recent-first (index 0 is what the login pages prefill).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialHistory {
    pub user_names: Vec<String>,
    pub passwords: Vec<String>,
}

impl CredentialHistory {
    pub fn from_settings(email_value: &str, pass_value: &str) -> Self {
        Self {
            user_names: split_list_setting(email_value),
            passwords: split_list_setting(pass_value),
        }
    }

    /// The two setting values, (email, pass).
    pub fn to_settings(&self) -> (String, String) {
        (
            join_list_setting(&self.user_names),
            join_list_setting(&self.passwords),
        )
    }

    /// Index-paired entries, as the original's Dictionary rebuild
    /// sees them; a length mismatch truncates to the shorter side
    /// here (the original *throws* while rebuilding — see
    /// [`Self::record_login`] for where that matters).
    pub fn pairs(&self) -> Vec<(&str, &str)> {
        self.user_names
            .iter()
            .zip(self.passwords.iter())
            .map(|(u, p)| (u.as_str(), p.as_str()))
            .collect()
    }

    /// The save a finished server login performs
    /// (`McLoginServerStart`): rebuild the user→password map from the
    /// stored lists, drop the current user, re-insert it at the
    /// front. If the stored lists are malformed (length mismatch or
    /// a duplicated user name), the original's rebuild throws and its
    /// catch block clears *both* settings — mirrored here by
    /// returning an empty history (the just-finished login is lost
    /// too, exactly as in the original).
    pub fn record_login(&mut self, user_name: &str, password: &str) {
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut malformed = self.user_names.len() != self.passwords.len();
        if !malformed {
            for (user, pass) in self.user_names.iter().zip(self.passwords.iter()) {
                if pairs.iter().any(|(existing, _)| existing == user) {
                    malformed = true;
                    break;
                }
                pairs.push((user.clone(), pass.clone()));
            }
        }
        if malformed {
            self.user_names.clear();
            self.passwords.clear();
            return;
        }
        pairs.retain(|(user, _)| user != user_name);
        pairs.insert(0, (user_name.to_string(), password.to_string()));
        self.user_names = pairs.iter().map(|(user, _)| user.clone()).collect();
        self.passwords = pairs.iter().map(|(_, pass)| pass.clone()).collect();
    }

    /// Remove one remembered account (the delete button handler in
    /// `Application.xaml.vb`).
    pub fn remove(&mut self, user_name: &str) {
        let pairs: Vec<(String, String)> = self
            .pairs()
            .into_iter()
            .filter(|(user, _)| *user != user_name)
            .map(|(user, pass)| (user.to_string(), pass.to_string()))
            .collect();
        self.user_names = pairs.iter().map(|(user, _)| user.clone()).collect();
        self.passwords = pairs.iter().map(|(_, pass)| pass.clone()).collect();
    }
}

// ---------------------------------------------------------------------------
// AccountStore — the flat settings shape (Settings.vb account keys).
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

fn default_ms_json() -> String {
    "{}".to_string()
}

/// All account-related settings, in the flat shape PCL keeps them
/// (one registry value per key on Windows; the serde names are the
/// setting keys, so a serialized store is a key→value map just like
/// the original's storage). Defaults match `Settings.vb`.
///
/// The per-instance overrides PCL also supports
/// (`VersionServerAuthServer`, `VersionServerNide`,
/// `VersionServerAuthRegister`, stored per instance) are a different
/// store and are not part of this global shape; the base-URL builders
/// accept whichever address applies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountStore {
    /// `LoginType` — the active login type.
    #[serde(rename = "LoginType", default)]
    pub login_type: McLoginType,
    /// `LoginPageType` — which login page is shown.
    #[serde(rename = "LoginPageType", default)]
    pub login_page_type: i32,
    /// `LoginRemember` — remember server-login credentials.
    #[serde(rename = "LoginRemember", default = "default_true")]
    pub login_remember: bool,

    // --- Microsoft, V2 cache (CacheMsV2*) ---
    #[serde(rename = "CacheMsV2OAuthRefresh", default)]
    pub ms_oauth_refresh: String,
    #[serde(rename = "CacheMsV2Access", default)]
    pub ms_access: String,
    #[serde(rename = "CacheMsV2ProfileJson", default)]
    pub ms_profile_json: String,
    #[serde(rename = "CacheMsV2Uuid", default)]
    pub ms_uuid: String,
    #[serde(rename = "CacheMsV2Name", default)]
    pub ms_name: String,
    /// Access-token expiry, Unix seconds (`CacheMsV2Expires`, a Long).
    #[serde(rename = "CacheMsV2Expires", default)]
    pub ms_expires: i64,
    /// `CacheMsV2Migrated` — the V1→V2 migration has run.
    #[serde(rename = "CacheMsV2Migrated", default)]
    pub ms_v2_migrated: bool,

    // --- Microsoft, V1 cache (CacheMs*, pre-migration) ---
    #[serde(rename = "CacheMsOAuthRefresh", default)]
    pub ms_v1_oauth_refresh: String,
    #[serde(rename = "CacheMsAccess", default)]
    pub ms_v1_access: String,
    #[serde(rename = "CacheMsProfileJson", default)]
    pub ms_v1_profile_json: String,
    #[serde(rename = "CacheMsUuid", default)]
    pub ms_v1_uuid: String,
    #[serde(rename = "CacheMsName", default)]
    pub ms_v1_name: String,

    /// `LoginMsJson` — the Microsoft account list, as the raw JSON
    /// string PCL stores; use [`Self::ms_accounts`] for the parsed
    /// form.
    #[serde(rename = "LoginMsJson", default = "default_ms_json")]
    pub ms_json: String,

    /// `LoginLegacyName` — offline player names, ¨-joined,
    /// most-recent-first; use [`Self::legacy_names`].
    #[serde(rename = "LoginLegacyName", default)]
    pub legacy_name: String,

    // --- Nide cache (CacheNide*) ---
    #[serde(rename = "CacheNideAccess", default)]
    pub nide_access: String,
    #[serde(rename = "CacheNideClient", default)]
    pub nide_client: String,
    #[serde(rename = "CacheNideUuid", default)]
    pub nide_uuid: String,
    #[serde(rename = "CacheNideName", default)]
    pub nide_name: String,
    #[serde(rename = "CacheNideUsername", default)]
    pub nide_user_name: String,
    #[serde(rename = "CacheNidePass", default)]
    pub nide_password: String,
    /// The Nide server id used to build the base URL.
    #[serde(rename = "CacheNideServer", default)]
    pub nide_server: String,
    /// `LoginNideEmail` / `LoginNidePass` — remembered logins.
    #[serde(rename = "LoginNideEmail", default)]
    pub nide_email_history: String,
    #[serde(rename = "LoginNidePass", default)]
    pub nide_pass_history: String,

    // --- Auth (authlib-injector) cache (CacheAuth*) ---
    #[serde(rename = "CacheAuthAccess", default)]
    pub auth_access: String,
    #[serde(rename = "CacheAuthClient", default)]
    pub auth_client: String,
    #[serde(rename = "CacheAuthUuid", default)]
    pub auth_uuid: String,
    #[serde(rename = "CacheAuthName", default)]
    pub auth_name: String,
    #[serde(rename = "CacheAuthUsername", default)]
    pub auth_user_name: String,
    #[serde(rename = "CacheAuthPass", default)]
    pub auth_password: String,
    /// The auth server's base address (without `/authserver`).
    #[serde(rename = "CacheAuthServerServer", default)]
    pub auth_server: String,
    /// The auth server's display name.
    #[serde(rename = "CacheAuthServerName", default)]
    pub auth_server_name: String,
    /// The auth server's registration URL.
    #[serde(rename = "CacheAuthServerRegister", default)]
    pub auth_server_register: String,
    /// `LoginAuthEmail` / `LoginAuthPass` — remembered logins.
    #[serde(rename = "LoginAuthEmail", default)]
    pub auth_email_history: String,
    #[serde(rename = "LoginAuthPass", default)]
    pub auth_pass_history: String,
}

impl Default for AccountStore {
    fn default() -> Self {
        Self {
            login_type: McLoginType::Legacy,
            login_page_type: 0,
            login_remember: true,
            ms_oauth_refresh: String::new(),
            ms_access: String::new(),
            ms_profile_json: String::new(),
            ms_uuid: String::new(),
            ms_name: String::new(),
            ms_expires: 0,
            ms_v2_migrated: false,
            ms_v1_oauth_refresh: String::new(),
            ms_v1_access: String::new(),
            ms_v1_profile_json: String::new(),
            ms_v1_uuid: String::new(),
            ms_v1_name: String::new(),
            ms_json: default_ms_json(),
            legacy_name: String::new(),
            nide_access: String::new(),
            nide_client: String::new(),
            nide_uuid: String::new(),
            nide_name: String::new(),
            nide_user_name: String::new(),
            nide_password: String::new(),
            nide_server: String::new(),
            nide_email_history: String::new(),
            nide_pass_history: String::new(),
            auth_access: String::new(),
            auth_client: String::new(),
            auth_uuid: String::new(),
            auth_name: String::new(),
            auth_user_name: String::new(),
            auth_password: String::new(),
            auth_server: String::new(),
            auth_server_name: String::new(),
            auth_server_register: String::new(),
            auth_email_history: String::new(),
            auth_pass_history: String::new(),
        }
    }
}

impl AccountStore {
    /// The parsed Microsoft account list. A malformed `LoginMsJson`
    /// makes the original's login page reset the setting to "{}";
    /// this accessor surfaces the error instead of hiding it.
    pub fn ms_accounts(&self) -> Result<MsAccountList, serde_json::Error> {
        MsAccountList::parse(&self.ms_json)
    }

    /// Write an account list back in PCL's compact form.
    pub fn set_ms_accounts(&mut self, accounts: &MsAccountList) {
        self.ms_json = accounts.to_json();
    }

    /// The offline player-name history, most-recent-first.
    pub fn legacy_names(&self) -> Vec<String> {
        split_list_setting(&self.legacy_name)
    }

    pub fn set_legacy_names(&mut self, names: &[String]) {
        self.legacy_name = join_list_setting(names);
    }

    /// The write a finished offline login performs
    /// (`McLoginLegacyStart`): the name moves to the front of the
    /// history (duplicates removed).
    pub fn record_legacy_login(&mut self, user_name: &str) {
        let mut names = self.legacy_names();
        names.retain(|name| name != user_name);
        names.insert(0, user_name.to_string());
        self.set_legacy_names(&names);
    }

    pub fn nide_history(&self) -> CredentialHistory {
        CredentialHistory::from_settings(&self.nide_email_history, &self.nide_pass_history)
    }

    pub fn set_nide_history(&mut self, history: &CredentialHistory) {
        let (email, pass) = history.to_settings();
        self.nide_email_history = email;
        self.nide_pass_history = pass;
    }

    pub fn auth_history(&self) -> CredentialHistory {
        CredentialHistory::from_settings(&self.auth_email_history, &self.auth_pass_history)
    }

    pub fn set_auth_history(&mut self, history: &CredentialHistory) {
        let (email, pass) = history.to_settings();
        self.auth_email_history = email;
        self.auth_pass_history = pass;
    }

    /// The Microsoft login input built from the V2 cache — the full
    /// `PageLoginMsSkin.GetLoginData` form (used once a session
    /// exists).
    pub fn ms_login_data(&self) -> McLoginMs {
        McLoginMs {
            login_type: McLoginType::Ms,
            oauth_refresh_token: self.ms_oauth_refresh.clone(),
            access_token: self.ms_access.clone(),
            uuid: self.ms_uuid.clone(),
            user_name: self.ms_name.clone(),
            profile_json: self.ms_profile_json.clone(),
        }
    }

    /// The server login input built from a type's cache — the
    /// `PageLogin{Auth,Nide}Skin.GetLoginData` form. Uses the global
    /// cached server address/id; callers with an instance override
    /// build via [`McLoginServer::auth`] / [`McLoginServer::nide`]
    /// instead, as the original does. None for Ms / Legacy.
    pub fn server_login_data(&self, login_type: McLoginType) -> Option<McLoginServer> {
        match login_type {
            McLoginType::Auth => Some(McLoginServer::auth(
                &self.auth_user_name,
                &self.auth_password,
                &self.auth_server,
            )),
            McLoginType::Nide => Some(McLoginServer::nide(
                &self.nide_user_name,
                &self.nide_password,
                &self.nide_server,
            )),
            _ => None,
        }
    }

    /// `McLoginName`: the current player's Minecraft name, or None.
    /// The active login type's cache wins; if it has no name, the
    /// caches are scanned in Ms → Nide → Auth → Legacy order. The
    /// Legacy entry is the first (most recent) history name.
    pub fn current_login_name(&self) -> Option<String> {
        let legacy_first = || -> Option<String> {
            if self.legacy_name.is_empty() {
                None
            } else {
                Some(before_first(&self.legacy_name, "¨"))
            }
        };
        match self.login_type {
            McLoginType::Ms if !self.ms_name.is_empty() => return Some(self.ms_name.clone()),
            McLoginType::Legacy => {
                if let Some(name) = legacy_first() {
                    return Some(name);
                }
            }
            McLoginType::Nide if !self.nide_name.is_empty() => {
                return Some(self.nide_name.clone())
            }
            McLoginType::Auth if !self.auth_name.is_empty() => {
                return Some(self.auth_name.clone())
            }
            _ => {}
        }
        if !self.ms_name.is_empty() {
            return Some(self.ms_name.clone());
        }
        if !self.nide_name.is_empty() {
            return Some(self.nide_name.clone());
        }
        if !self.auth_name.is_empty() {
            return Some(self.auth_name.clone());
        }
        legacy_first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_type_values_and_names() {
        assert_eq!(McLoginType::Legacy.value(), 0);
        assert_eq!(McLoginType::Nide.value(), 2);
        assert_eq!(McLoginType::Auth.value(), 3);
        assert_eq!(McLoginType::Ms.value(), 5);
        assert_eq!(McLoginType::from_value(1), None);
        assert_eq!(McLoginType::from_value(4), None);
        assert_eq!(McLoginType::Ms.name(), "Ms");
        // Serde is the numeric value, as in the registry.
        assert_eq!(serde_json::to_string(&McLoginType::Auth).unwrap(), "3");
        assert_eq!(
            serde_json::from_str::<McLoginType>("5").unwrap(),
            McLoginType::Ms
        );
        assert!(serde_json::from_str::<McLoginType>("7").is_err());
    }

    #[test]
    fn stable_hash_matches_djb_variant() {
        // Computed from the C# algorithm by hand for a tiny input:
        // state 5381, one UTF-16 unit 'a' (97):
        // (5381 << 5) ^ 5381 ^ 97, then ^ 0xA98F501BC684032F.
        let expected = ((5381u64 << 5) ^ 5381 ^ 97) ^ 0xA98F_501B_C684_032F;
        assert_eq!(stable_hash_code("a"), expected);
        assert_eq!(stable_hash_code(""), 5381 ^ 0xA98F_501B_C684_032F);
    }

    #[test]
    fn login_data_serde_dispatches_on_type() {
        // Ms (5)
        let data: McLoginData =
            serde_json::from_str(r#"{"Type":5,"OAuthRefreshToken":"fake-refresh","UserName":"Steve"}"#)
                .unwrap();
        let McLoginData::Ms(ms) = &data else {
            panic!("expected Ms, got {data:?}")
        };
        assert_eq!(ms.oauth_refresh_token, "fake-refresh");
        assert_eq!(ms.access_token, "");
        assert_eq!(
            serde_json::to_string(&data).unwrap(),
            r#"{"Type":5,"OAuthRefreshToken":"fake-refresh","AccessToken":"","Uuid":"","UserName":"Steve","ProfileJson":""}"#
        );
        // Legacy (0)
        let data: McLoginData =
            serde_json::from_str(r#"{"Type":0,"UserName":"Alex","SkinType":2,"SkinName":""}"#)
                .unwrap();
        assert!(matches!(data, McLoginData::Legacy(_)));
        // Server covers both Auth (3) and Nide (2).
        let data: McLoginData = serde_json::from_str(
            r#"{"Type":3,"UserName":"u@example.invalid","Password":"fake-pass","BaseUrl":"https://example.invalid/authserver","Token":"Auth","Description":"Authlib-Injector"}"#,
        )
        .unwrap();
        let McLoginData::Server(server) = &data else {
            panic!("expected Server, got {data:?}")
        };
        assert_eq!(server.login_type, McLoginType::Auth);
        assert!(!server.force_reselect_profile);
        let data: McLoginData =
            serde_json::from_str(r#"{"Type":2,"UserName":"u","Password":"p","Token":"Nide"}"#)
                .unwrap();
        assert!(matches!(data, McLoginData::Server(_)));
        // Unknown / missing Type is rejected.
        assert!(serde_json::from_str::<McLoginData>(r#"{"Type":4}"#).is_err());
        assert!(serde_json::from_str::<McLoginData>(r#"{"UserName":"x"}"#).is_err());
    }

    #[test]
    fn pcl_equality_is_hash_based() {
        // McLoginMs hashes only refresh token + user name: a different
        // access token still counts as the same login input.
        let a = McLoginMs {
            oauth_refresh_token: "fake-refresh".into(),
            user_name: "Steve".into(),
            access_token: "fake-access-1".into(),
            ..Default::default()
        };
        let b = McLoginMs {
            access_token: "fake-access-2".into(),
            ..a.clone()
        };
        assert_ne!(a, b); // structural comparison differs
        let (a, b) = (McLoginData::Ms(a), McLoginData::Ms(b));
        assert!(a.pcl_equals(&b));
        // A different user name changes the identity.
        let c = McLoginData::Ms(McLoginMs {
            oauth_refresh_token: "fake-refresh".into(),
            user_name: "Alex".into(),
            ..Default::default()
        });
        assert!(!a.pcl_equals(&c));
        // McLoginServer's identity includes the password and base URL.
        let s1 = McLoginServer::auth("u@example.invalid", "fake-pass", "https://example.invalid");
        let s2 = McLoginServer::auth("u@example.invalid", "other-pass", "https://example.invalid");
        assert!(!McLoginData::Server(s1).pcl_equals(&McLoginData::Server(s2)));
    }

    #[test]
    fn validate_messages_match_original() {
        assert_eq!(
            McLoginMs::default().validate(),
            Err("请在登录账号后再启动游戏！")
        );
        let server = McLoginServer::nide("", "", "4242");
        assert_eq!(server.validate(), Err("账号不能为空！"));
        let server = McLoginServer::nide("user", "", "4242");
        assert_eq!(server.validate(), Err("密码不能为空！"));
        assert!(McLoginServer::nide("user", "fake-pass", "4242")
            .validate()
            .is_ok());

        let legacy = McLoginLegacy::default();
        assert_eq!(legacy.validate(), Err("玩家名不能为空！"));
        let legacy = McLoginLegacy {
            user_name: "Bad\"Name".into(),
            ..Default::default()
        };
        assert_eq!(legacy.validate(), Err("玩家名不能包含英文引号！"));
        // The 1.20.3 length rule only fires with a valid, new-enough
        // vanilla version supplied.
        let legacy = McLoginLegacy {
            user_name: "a".repeat(17),
            ..Default::default()
        };
        assert!(legacy.validate().is_ok());
        assert!(legacy
            .validate_for_version(Some(crate::version::name_to_version("1.20.2")))
            .is_ok());
        assert_eq!(
            legacy.validate_for_version(Some(crate::version::name_to_version("1.20.3"))),
            Err("自 1.20.3 起，玩家名至多只能包含 16 个字符！")
        );
    }

    #[test]
    fn base_url_builders() {
        assert_eq!(
            auth_base_url("https://example.invalid"),
            "https://example.invalid/authserver"
        );
        assert_eq!(
            nide_base_url("4242"),
            "https://auth.mc-user.com:233/4242/authserver"
        );
        let server = McLoginServer::auth("u", "p", "https://example.invalid");
        assert_eq!(server.token, "Auth");
        assert_eq!(server.description, "Authlib-Injector");
        let server = McLoginServer::nide("u", "p", "4242");
        assert_eq!(server.token, "Nide");
        assert_eq!(server.description, "统一通行证");
    }

    #[test]
    fn ms_account_list_order_and_record() {
        let mut list = MsAccountList::parse(r#"{"Steve":"fake-tok-a","Alex":"fake-tok-b"}"#).unwrap();
        assert_eq!(list.get("Alex"), Some("fake-tok-b"));
        // Renamed player: old name removed, new name appended.
        list.record_login("Steve", "SteveNew", "fake-tok-c");
        assert_eq!(list.get("Steve"), None);
        assert_eq!(
            list.to_json(),
            r#"{"Alex":"fake-tok-b","SteveNew":"fake-tok-c"}"#
        );
        // Setting an existing name replaces in place (order kept).
        list.set("Alex", "fake-tok-d");
        assert_eq!(
            list.to_json(),
            r#"{"Alex":"fake-tok-d","SteveNew":"fake-tok-c"}"#
        );
        list.remove("Alex");
        assert_eq!(list.to_json(), r#"{"SteveNew":"fake-tok-c"}"#);
        // The empty default.
        assert_eq!(MsAccountList::default().to_json(), "{}");
        assert!(MsAccountList::parse("not json").is_err());
    }

    #[test]
    fn credential_history_record_and_malformed_wipe() {
        let mut history = CredentialHistory::from_settings(
            "b@example.invalid¨a@example.invalid",
            "fake-pw-b¨fake-pw-a",
        );
        history.record_login("c@example.invalid", "fake-pw-c");
        assert_eq!(
            history.to_settings(),
            (
                "c@example.invalid¨b@example.invalid¨a@example.invalid".to_string(),
                "fake-pw-c¨fake-pw-b¨fake-pw-a".to_string()
            )
        );
        // Re-login moves the account to the front with the new password.
        history.record_login("a@example.invalid", "fake-pw-a2");
        assert_eq!(history.user_names[0], "a@example.invalid");
        assert_eq!(history.passwords[0], "fake-pw-a2");
        assert_eq!(history.user_names.len(), 3);
        history.remove("b@example.invalid");
        assert_eq!(history.user_names.len(), 2);
        // Malformed stored lists (count mismatch) are wiped wholesale
        // by the original's catch block — the new login is lost too.
        let mut bad = CredentialHistory::from_settings("a¨b", "only-one");
        bad.record_login("c", "fake-pw");
        assert!(bad.user_names.is_empty() && bad.passwords.is_empty());
        // Duplicated user names are malformed the same way.
        let mut dup = CredentialHistory::from_settings("a¨a", "x¨y");
        dup.record_login("c", "fake-pw");
        assert!(dup.user_names.is_empty());
    }

    #[test]
    fn account_store_defaults_and_roundtrip() {
        let store = AccountStore::default();
        assert_eq!(store.login_type, McLoginType::Legacy);
        assert!(store.login_remember);
        assert_eq!(store.ms_json, "{}");
        assert_eq!(store.ms_expires, 0);
        // An empty object deserializes to the same defaults (every
        // field has a default, as registry reads fall back per key).
        let parsed: AccountStore = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, store);
        // Field names on the wire are the PCL setting keys.
        let text = serde_json::to_string(&store).unwrap();
        assert!(text.contains("\"LoginType\":0"), "{text}");
        assert!(text.contains("\"CacheMsV2Expires\":0"), "{text}");
        assert!(text.contains("\"LoginMsJson\":\"{}\""), "{text}");
        let back: AccountStore = serde_json::from_str(&text).unwrap();
        assert_eq!(back, store);
    }

    #[test]
    fn account_store_typed_accessors() {
        let mut store = AccountStore::default();
        // Legacy history: most recent first, re-login bubbles up.
        store.record_legacy_login("Alex");
        store.record_legacy_login("Steve");
        store.record_legacy_login("Alex");
        assert_eq!(store.legacy_name, "Alex¨Steve");
        assert_eq!(store.legacy_names(), vec!["Alex", "Steve"]);
        // MS accounts through the raw JSON string.
        let mut accounts = store.ms_accounts().unwrap();
        accounts.record_login("", "Steve", "fake-refresh");
        store.set_ms_accounts(&accounts);
        assert_eq!(store.ms_json, r#"{"Steve":"fake-refresh"}"#);
        // Cache-derived login inputs.
        store.ms_oauth_refresh = "fake-refresh".into();
        store.ms_name = "Steve".into();
        store.ms_uuid = "fake-uuid".into();
        let ms = store.ms_login_data();
        assert_eq!(ms.user_name, "Steve");
        assert!(ms.validate().is_ok());
        store.auth_user_name = "u@example.invalid".into();
        store.auth_password = "fake-pass".into();
        store.auth_server = "https://example.invalid".into();
        let server = store.server_login_data(McLoginType::Auth).unwrap();
        assert_eq!(server.base_url, "https://example.invalid/authserver");
        assert!(store.server_login_data(McLoginType::Ms).is_none());
    }

    #[test]
    fn current_login_name_priority() {
        let mut store = AccountStore::default();
        assert_eq!(store.current_login_name(), None);
        // Active type Auth but no Auth name cached: fall back to the
        // scan order (Ms first).
        store.login_type = McLoginType::Auth;
        store.ms_name = "MsPlayer".into();
        store.nide_name = "NidePlayer".into();
        assert_eq!(store.current_login_name().as_deref(), Some("MsPlayer"));
        // Active type with a name wins immediately.
        store.auth_name = "AuthPlayer".into();
        assert_eq!(store.current_login_name().as_deref(), Some("AuthPlayer"));
        // Legacy resolves to the most recent history name.
        store.login_type = McLoginType::Legacy;
        store.legacy_name = "Recent¨Older".into();
        assert_eq!(store.current_login_name().as_deref(), Some("Recent"));
    }
}
