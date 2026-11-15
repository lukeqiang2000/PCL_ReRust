//! Live login flows on top of the M1 account model.
//!
//! Ports of `ModLaunch.vb`'s Microsoft chain
//! (`McLoginMsStart` + `MsLoginStep1New/1Refresh/2..6`), the
//! dialog-side device-code poll in `MyMsgLogin.xaml.vb`, the
//! Legacy offline login (`McLoginLegacyStart`) and the yggdrasil
//! server login (`McLoginServerStart` +
//! `McLoginRequestValidate/Refresh/Login`).
//!
//! The Microsoft chain, exactly as upstream:
//! 0. Cache: when `CacheMsV2Expires` is in the future and the
//!    requested user name equals `CacheMsV2Name`, the cached
//!    session is returned with no network traffic.
//! 1a. No refresh token → device-code flow against
//!     `login.microsoftonline.com/consumers/oauth2/v2.0`: POST
//!     `devicecode` (scope `XboxLive.signin offline_access`), the
//!     user authorizes in a browser, the client polls `token`
//!     (2 s cadence, `authorization_pending` keeps polling, error
//!     strings map to user-facing failures / `Restart`).
//! 1b. With a refresh token → POST
//!     `login.live.com/oauth20_token.srf` (scope
//!     `service::user.auth.xboxlive.com::MBI_SSL`); specific error
//!     strings downgrade to "Relogin" (back to 1a).
//! 2. XBL: POST `user.auth.xboxlive.com/user/authenticate` with
//!    `RpsTicket = "d=" + oauth_access_token`.
//! 3. XSTS: POST `xsts.auth.xboxlive.com/xsts/authorize`
//!    (RelyingParty `rp://api.minecraftservices.com/`); XErr codes
//!    map to the upstream guidance texts.
//! 4. Minecraft: POST
//!    `api.minecraftservices.com/authentication/login_with_xbox`
//!    with `XBL3.0 x={uhs};{xsts_token}`; the stored expiry is
//!    `now + expires_in - 3600` (upstream refreshes an hour early).
//! 5. Entitlements: GET `entitlements/mcstore` — an empty `items`
//!    list means the account does not own the game.
//! 6. Profile: GET `minecraft/profile` — 404 means no profile yet.
//!    The result's ClientToken is the profile UUID (upstream
//!    quirk), and the raw profile JSON is kept verbatim.
//!
//! The OAuth client id comes from the `PCL_MS_CLIENT_ID`
//! environment variable (`ModSecret.vb`: the open-source build
//! leaves it empty; private builds inject one). No real credential
//! is ever used by this crate's tests — the chain is exercised
//! against scripted fake responses, and real sign-in is left for
//! the user to try.

use std::time::Duration;

use serde_json::{json, Value};

use crate::account::{AccountStore, McLoginLegacy, McLoginMs, McLoginResult, McLoginServer};
use crate::http::{HttpClient, HttpError, HttpRequest};

/// OAuth client id from the environment (`ModSecret.OAuthClientId`).
pub fn oauth_client_id() -> String {
    std::env::var("PCL_MS_CLIENT_ID").unwrap_or_default()
}

/// Login failure, keeping upstream's control-flow exceptions
/// distinguishable from plain errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginError {
    /// A user-facing failure message (upstream `Exception` with a
    /// `$`-prefixed display string, shown without the prefix).
    Message(String),
    /// Upstream `RestartException`: the caller should restart the
    /// whole login from step 1.
    Restart,
    /// The stored refresh token is dead; fall back to a fresh
    /// device-code login (upstream's `"Relogin"` sentinel).
    Relogin,
    /// The user cancelled (upstream `OperationCanceledException`).
    Cancelled,
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::Message(m) => write!(f, "{m}"),
            LoginError::Restart => write!(f, "登录需要重新开始"),
            LoginError::Relogin => write!(f, "登录已过期，需要重新登录"),
            LoginError::Cancelled => write!(f, "登录已取消"),
        }
    }
}

impl std::error::Error for LoginError {}

fn msg(text: &str) -> LoginError {
    LoginError::Message(text.to_string())
}

/// What the user must do in the browser during a device-code login
/// (the `MyMsgLogin` dialog content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCodePrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub message: Option<String>,
}

/// Knobs for the MS chain (poll cadence is injectable so tests do
/// not sleep).
#[derive(Debug, Clone)]
pub struct MsLoginOptions {
    pub client_id: String,
    /// Sleep between token polls (upstream: a flat 2 s).
    pub poll_interval: Duration,
    /// Give up polling after this many attempts (upstream polls
    /// until the dialog closes or the code expires; the port
    /// bounds it so a stuck login cannot spin forever).
    pub max_poll_attempts: u32,
}

impl Default for MsLoginOptions {
    fn default() -> Self {
        MsLoginOptions {
            client_id: oauth_client_id(),
            poll_interval: Duration::from_secs(2),
            max_poll_attempts: 300,
        }
    }
}

/// A completed Microsoft login: the launch-facing result plus the
/// values the caller must persist (`CacheMsV2OAuthRefresh`,
/// `CacheMsV2Expires`, and the account-list entry — see
/// `McLoginMsStart`'s Settings writes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsSession {
    pub result: McLoginResult,
    pub oauth_refresh_token: String,
    pub expires_unix: i64,
}

/// The cache shortcut of `McLoginMsStart`: when the stored expiry
/// is in the future and the requested name matches the cached one,
/// the session is rebuilt from the `CacheMsV2*` fields verbatim.
pub fn ms_cached_result(
    store: &AccountStore,
    input: &McLoginMs,
    now_unix: i64,
) -> Option<McLoginResult> {
    if store.ms_expires > 0 && store.ms_expires > now_unix && input.user_name == store.ms_name {
        Some(McLoginResult {
            name: input.user_name.clone(),
            uuid: store.ms_uuid.clone(),
            access_token: store.ms_access.clone(),
            result_type: "Microsoft".to_string(),
            client_token: store.ms_uuid.clone(),
            profile_json: store.ms_profile_json.clone(),
        })
    } else {
        None
    }
}

/// Full Microsoft login (`McLoginMsStart`): cache first, then the
/// refresh-token path when one is stored, else the device-code
/// flow; steps 2–6 always run on a fresh OAuth token.
pub fn ms_login(
    http: &dyn HttpClient,
    options: &MsLoginOptions,
    input: &McLoginMs,
    store: &AccountStore,
    now_unix: i64,
    on_prompt: &mut dyn FnMut(&DeviceCodePrompt),
) -> Result<MsSession, LoginError> {
    if let Some(result) = ms_cached_result(store, input, now_unix) {
        return Ok(MsSession {
            result,
            oauth_refresh_token: input.oauth_refresh_token.clone(),
            expires_unix: store.ms_expires,
        });
    }
    let (oauth_access, oauth_refresh) = if input.oauth_refresh_token.is_empty() {
        ms_step1_new(http, options, on_prompt)?
    } else {
        match ms_step1_refresh(http, options, &input.oauth_refresh_token) {
            Ok(pair) => pair,
            Err(LoginError::Relogin) => ms_step1_new(http, options, on_prompt)?,
            Err(e) => return Err(e),
        }
    };
    ms_steps_2_to_6(http, &oauth_access, &oauth_refresh, now_unix)
}

/// Step 1 (new): the device-code flow. Returns
/// (oauth access token, oauth refresh token).
pub fn ms_step1_new(
    http: &dyn HttpClient,
    options: &MsLoginOptions,
    on_prompt: &mut dyn FnMut(&DeviceCodePrompt),
) -> Result<(String, String), LoginError> {
    if options.client_id.is_empty() {
        return Err(msg(
            "未配置微软 OAuth 客户端 ID（环境变量 PCL_MS_CLIENT_ID），无法进行正版登录",
        ));
    }
    let device_req = HttpRequest::post_form(
        "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode",
        &[
            ("client_id", options.client_id.as_str()),
            ("scope", "XboxLive.signin offline_access"),
        ],
    );
    let device_resp = http
        .request_retry(&device_req)
        .map_err(|e| msg(&format!("获取登录代码失败：{e}")))?;
    let device_json = parse_json(&device_resp.text(), "获取登录代码失败")?;
    let prompt = DeviceCodePrompt {
        user_code: json_string(&device_json, "user_code"),
        verification_uri: json_string(&device_json, "verification_uri"),
        message: device_json
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let device_code = json_string(&device_json, "device_code");
    on_prompt(&prompt);
    // Poll for authorization (MyMsgLogin.WorkThread).
    let mut unknown_failures = 0u32;
    for _ in 0..options.max_poll_attempts {
        std::thread::sleep(options.poll_interval);
        let poll_req = HttpRequest::post_form(
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", options.client_id.as_str()),
                ("device_code", device_code.as_str()),
                ("scope", "XboxLive.signin offline_access"),
            ],
        );
        match http.request(&poll_req) {
            Ok(resp) => {
                let token_json = parse_json(&resp.text(), "登录轮询失败")?;
                return Ok((
                    json_string(&token_json, "access_token"),
                    json_string(&token_json, "refresh_token"),
                ));
            }
            Err(HttpError::Status { body, .. }) => {
                let text = body;
                if text.contains("authorization_pending") {
                    continue;
                } else if text.contains("authorization_declined") {
                    return Err(msg("你拒绝了 PCL 申请的权限……"));
                } else if text.contains("expired_token") {
                    return Err(msg("登录用时太长啦，重新试试吧！"));
                } else if text.contains("Account security interrupt") {
                    return Err(msg(
                        "该账号由于安全问题无法登陆，请前往微软账户页获取更多信息。",
                    ));
                } else if text.contains("service abuse") {
                    return Err(msg("非常抱歉，该账号已被微软封禁，无法登录。"));
                } else if text.contains("AADSTS70000") {
                    return Err(LoginError::Restart);
                }
                unknown_failures += 1;
                if unknown_failures > 2 {
                    return Err(msg(&format!("登录轮询失败：{text}")));
                }
            }
            Err(e) => {
                unknown_failures += 1;
                if unknown_failures > 2 {
                    return Err(msg(&format!("登录轮询失败：{e}")));
                }
            }
        }
    }
    Err(msg("登录用时太长啦，重新试试吧！"))
}

/// Step 1 (refresh): exchange the stored OAuth refresh token at
/// login.live.com. Specific upstream error strings downgrade to
/// [`LoginError::Relogin`].
pub fn ms_step1_refresh(
    http: &dyn HttpClient,
    options: &MsLoginOptions,
    refresh_token: &str,
) -> Result<(String, String), LoginError> {
    let req = HttpRequest::post_form(
        "https://login.live.com/oauth20_token.srf",
        &[
            ("client_id", options.client_id.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
            ("scope", "service::user.auth.xboxlive.com::MBI_SSL"),
        ],
    );
    match http.request_retry(&req) {
        Ok(resp) => {
            let token_json = parse_json(&resp.text(), "刷新登录失败")?;
            Ok((
                json_string(&token_json, "access_token"),
                json_string(&token_json, "refresh_token"),
            ))
        }
        Err(e) => {
            let text = e.to_string();
            const RELOGIN_MARKERS: &[&str] = &[
                "account is in a restricted state",
                "The user has denied access to the scope requested by this application",
                "The application is not consented by the user",
                "invalid_grant",
                "The token has expired",
                "failed to validate the user credential",
                "Could not determine token type",
                "Cannot convert token",
                "We couldn't sign you in",
            ];
            if RELOGIN_MARKERS.iter().any(|m| text.contains(m)) {
                Err(LoginError::Relogin)
            } else {
                Err(msg(&format!("刷新 AccessToken 失败：{text}")))
            }
        }
    }
}

/// Steps 2–6 shared by the new and refresh paths.
fn ms_steps_2_to_6(
    http: &dyn HttpClient,
    oauth_access_token: &str,
    oauth_refresh_token: &str,
    now_unix: i64,
) -> Result<MsSession, LoginError> {
    // Step 2: Xbox Live authentication.
    let xbl_req = HttpRequest::post_json(
        "https://user.auth.xboxlive.com/user/authenticate",
        &json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={oauth_access_token}"),
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        })
        .to_string(),
    );
    let xbl_resp = http
        .request_retry(&xbl_req)
        .map_err(|e| msg(&format!("Xbox Live 身份验证失败（Step 2）：{e}")))?;
    let xbl_json = parse_json(&xbl_resp.text(), "Xbox Live 身份验证失败（Step 2）")?;
    let xbl_token = json_string(&xbl_json, "Token");
    let uhs = xbl_json
        .get("DisplayClaims")
        .and_then(|c| c.get("xui"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|x| x.get("uhs"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // Step 3: XSTS authorization.
    let xsts_req = HttpRequest::post_json(
        "https://xsts.auth.xboxlive.com/xsts/authorize",
        &json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [xbl_token],
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT",
        })
        .to_string(),
    );
    let xsts_resp = http.request_retry(&xsts_req).map_err(|e| {
        if let HttpError::Status { body, .. } = &e {
            if let Some(xerr) = extract_xerr(body) {
                return xsts_error_message(xerr);
            }
        }
        msg(&format!("Minecraft 身份验证失败（Step 3）：{e}"))
    })?;
    let xsts_json = parse_json(&xsts_resp.text(), "Minecraft 身份验证失败（Step 3）")?;
    let xsts_token = json_string(&xsts_json, "Token");

    // Step 4: Minecraft Services login.
    let mc_req = HttpRequest::post_json(
        "https://api.minecraftservices.com/authentication/login_with_xbox",
        &json!({ "identityToken": format!("XBL3.0 x={uhs};{xsts_token}") }).to_string(),
    );
    let mc_resp = http
        .request_retry(&mc_req)
        .map_err(|e| msg(&format!("Minecraft 登录失败（Step 4）：{e}")))?;
    let mc_json = parse_json(&mc_resp.text(), "Minecraft 登录失败（Step 4）")?;
    let access_token = json_string(&mc_json, "access_token");
    let expires_in = mc_json.get("expires_in").and_then(Value::as_i64).unwrap_or(0);
    // Upstream stores the expiry one hour early.
    let expires_unix = now_unix + expires_in - 3600;

    // Step 5: ownership check.
    ms_step5_check_owns_game(http, &access_token)?;

    // Step 6: profile (raw JSON kept verbatim, as upstream).
    let (name, uuid, profile_json) = ms_step6_get_profile(http, &access_token)?;

    Ok(MsSession {
        result: McLoginResult {
            // Upstream quirk: the MS result's ClientToken is the UUID.
            client_token: uuid.clone(),
            name,
            uuid,
            access_token,
            result_type: "Microsoft".to_string(),
            profile_json,
        },
        oauth_refresh_token: oauth_refresh_token.to_string(),
        expires_unix,
    })
}

/// Step 5: the account must own Minecraft (non-empty entitlements).
pub fn ms_step5_check_owns_game(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<(), LoginError> {
    let req = HttpRequest::get("https://api.minecraftservices.com/entitlements/mcstore")
        .header("Authorization", &format!("Bearer {access_token}"));
    let resp = http
        .request_retry(&req)
        .map_err(|e| msg(&format!("检查游戏所有权失败（Step 5）：{e}")))?;
    let entitlements = parse_json(&resp.text(), "检查游戏所有权失败（Step 5）")?;
    let items = entitlements
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return Err(msg(
            "该账号未购买 Minecraft 游戏，无法登录！\r\n你可以使用其他购买了游戏的账号登录，或在微软商店购买 Minecraft 后再登录。",
        ));
    }
    Ok(())
}

/// Step 6: fetch the Minecraft profile. Returns (name, uuid, raw
/// profile JSON). A 404 maps to the upstream "no profile" guidance.
pub fn ms_step6_get_profile(
    http: &dyn HttpClient,
    access_token: &str,
) -> Result<(String, String, String), LoginError> {
    let req = HttpRequest::get("https://api.minecraftservices.com/minecraft/profile")
        .header("Authorization", &format!("Bearer {access_token}"));
    let resp = http.request_retry(&req).map_err(|e| {
        if let HttpError::Status { status, .. } = e {
            if status == 404 {
                return msg("该账号尚未创建档案，无法登录！\r\n请登录 Minecraft 官网购买游戏并创建档案，然后再使用 PCL 登录。");
            }
        }
        msg(&format!("获取 Minecraft 档案失败（Step 6）：{e}"))
    })?;
    let text = resp.text();
    let profile = parse_json(&text, "获取 Minecraft 档案失败（Step 6）")?;
    Ok((
        json_string(&profile, "name"),
        json_string(&profile, "id"),
        text,
    ))
}

/// Extract an XSTS `XErr` code from an error body.
fn extract_xerr(text: &str) -> Option<i64> {
    let json: Value = serde_json::from_str(text).ok()?;
    json.get("XErr").and_then(Value::as_i64)
}

/// The upstream XSTS error guidance (`MsLoginStep3`).
fn xsts_error_message(xerr: i64) -> LoginError {
    match xerr {
        2148916227 => msg("该账号已被封禁，无法登录到 Xbox Live 账户！"),
        2148916233 => msg("该账号没有 Xbox Live 账户，无法登录！\r\n请打开 https://www.xbox.com 并登录以创建 Xbox Live 账户。"),
        2148916235 => msg("该账号所在国家或地区不可用 Xbox Live，无法登录！\r\n修改系统时区与语言设置可以解决此问题。"),
        2148916238 => msg("该账号未成年，未经家长同意无法登录！\r\n请尝试将该账号加入已成年的微软家庭组（教程：https://pan.meloong.com/d/1UontA）"),
        _ => msg(&format!(
            "Minecraft 身份验证失败，未知错误（{xerr}）：请检查网络设置和系统时间是否正确。"
        )),
    }
}

fn parse_json(text: &str, what: &str) -> Result<Value, LoginError> {
    serde_json::from_str(text).map_err(|e| msg(&format!("{what}：返回内容解析失败（{e}）")))
}

fn json_string(json: &Value, key: &str) -> String {
    json.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// Legacy (offline) login — McLoginLegacyStart and its UUID helpers
// (ModLaunch.vb: McLoginLegacyUuid / McLoginLegacyUuidWithCustomSkin,
// ModMinecraft.vb: McSkinSex). These live in the login module
// upstream, so the port keeps them here too.
// ---------------------------------------------------------------------------

/// `McLoginLegacyUuid`: PCL's own offline UUID algorithm — NOT the
/// standard MD5-based offline UUID. The first half is the UTF-16
/// length of the name in hex, the second half the stable hash, each
/// `EnsureLength`-ed to 16 (left-pad "0", truncate to the first
/// 16); then position 12 becomes "3" and position 16 becomes "9".
pub fn legacy_uuid(name: &str) -> String {
    fn ensure16(hex: String) -> String {
        if hex.len() > 16 {
            hex[..16].to_string()
        } else {
            format!("{:0>16}", hex)
        }
    }
    let len_hex = format!("{:X}", name.encode_utf16().count());
    let hash_hex = format!("{:X}", crate::account::stable_hash_code(name));
    let full = format!("{}{}", ensure16(len_hex), ensure16(hash_hex));
    let bytes = full.as_bytes();
    let mut out = String::with_capacity(32);
    out.push_str(std::str::from_utf8(&bytes[..12]).unwrap_or_default());
    out.push('3');
    out.push_str(std::str::from_utf8(&bytes[13..16]).unwrap_or_default());
    out.push('9');
    out.push_str(std::str::from_utf8(&bytes[17..32]).unwrap_or_default());
    out
}

/// `McSkinSex`: the skin model a UUID maps to — "Steve" or "Alex"
/// by the XOR parity of the hex digits at positions 7/15/23/31.
/// Non-32-char UUIDs are "Steve".
pub fn skin_sex(uuid: &str) -> &'static str {
    let bytes = uuid.as_bytes();
    if bytes.len() != 32 {
        return "Steve";
    }
    let digit = |i: usize| (bytes[i] as char).to_digit(16).unwrap_or(0);
    if (digit(7) ^ digit(15) ^ digit(23) ^ digit(31)) % 2 == 1 {
        "Alex"
    } else {
        "Steve"
    }
}

/// The increment loop of `McLoginLegacyUuidWithCustomSkin` types
/// 1/2/4: bump the last 5 hex digits (wrapping FFFFF → 00000, then
/// still incrementing, as upstream does) until the UUID's skin sex
/// is the target.
fn uuid_bump_to_sex(uuid: &str, target: &str) -> String {
    let mut uuid = uuid.to_string();
    while skin_sex(&uuid) != target {
        if uuid.ends_with("FFFFF") {
            uuid = format!("{}00000", &uuid[..27]);
        }
        let tail = i64::from_str_radix(&uuid[27..], 16).unwrap_or(0) + 1;
        uuid = format!("{}{:05X}", &uuid[..27], tail);
    }
    uuid
}

/// `McLoginLegacyUuidWithCustomSkin`. Skin types: 0 = default,
/// 1 = force Steve, 2 = force Alex, 3 = use a genuine player's
/// UUID (only when `skin3_allowed` — upstream requires a selected
/// instance with Vanilla major < 20, #3746 — and the lookup
/// succeeds; any failure silently keeps the offline UUID),
/// 4 = custom (`custom_slim` from the `LaunchSkinSlim` setting).
/// `mojang_uuid_of` is `McLoginMojangUuid`, injected so tests stay
/// offline.
pub fn legacy_uuid_with_skin(
    input: &McLoginLegacy,
    custom_slim: bool,
    skin3_allowed: bool,
    mojang_uuid_of: &dyn Fn(&str) -> Result<String, LoginError>,
) -> String {
    let uuid = legacy_uuid(&input.user_name);
    match input.skin_type {
        1 => uuid_bump_to_sex(&uuid, "Steve"),
        2 => uuid_bump_to_sex(&uuid, "Alex"),
        3 => {
            if skin3_allowed && !input.skin_name.is_empty() {
                mojang_uuid_of(&input.skin_name).unwrap_or(uuid)
            } else {
                uuid
            }
        }
        4 => uuid_bump_to_sex(&uuid, if custom_slim { "Alex" } else { "Steve" }),
        _ => uuid,
    }
}

/// `McLoginMojangUuid` against the live Mojang profile API (with
/// the upstream fallback: a missing profile yields that name's
/// offline UUID instead of an error). Provided for the CLI; unit
/// tests inject a fake instead.
pub fn mojang_uuid_online(http: &dyn HttpClient, name: &str) -> Result<String, LoginError> {
    if name.trim().is_empty() {
        return Ok("0".repeat(32));
    }
    let req = HttpRequest::get(&format!(
        "https://api.mojang.com/users/profiles/minecraft/{name}"
    ));
    match http.request_retry(&req) {
        Ok(resp) => {
            let body = parse_json(&resp.text(), "获取正版 UUID 失败")?;
            let id = json_string(&body, "id");
            if id.len() == 32 {
                Ok(id)
            } else {
                Err(msg("获取的正版 Uuid 长度不足"))
            }
        }
        Err(HttpError::Status { status, .. }) if status == 404 || status == 204 => {
            Ok(legacy_uuid(name))
        }
        Err(e) => Err(msg(&format!("从官网获取正版 Uuid 失败：{e}"))),
    }
}

/// `McLoginLegacyStart`: the offline result. AccessToken and
/// ClientToken are both the UUID (upstream).
pub fn legacy_login(
    input: &McLoginLegacy,
    custom_slim: bool,
    skin3_allowed: bool,
    mojang_uuid_of: &dyn Fn(&str) -> Result<String, LoginError>,
) -> Result<McLoginResult, LoginError> {
    if input.user_name.is_empty() {
        return Err(msg("用户名不能为空"));
    }
    let uuid = legacy_uuid_with_skin(input, custom_slim, skin3_allowed, mojang_uuid_of);
    Ok(McLoginResult {
        name: input.user_name.clone(),
        uuid: uuid.clone(),
        access_token: uuid.clone(),
        result_type: "Legacy".to_string(),
        client_token: uuid,
        profile_json: String::new(),
    })
}

// ---------------------------------------------------------------------------
// Yggdrasil server login — McLoginServerStart: cached session →
// validate → refresh → authenticate (→ refresh once more when the
// authenticate demands it, the upstream NeedRefresh path).
// ---------------------------------------------------------------------------

/// A previously stored server session (the `Cache{Token}*` fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCache {
    pub access_token: String,
    pub client_token: String,
    pub uuid: String,
    pub name: String,
}

/// The cached-session gate of `McLoginServerStart`: the stored
/// user name and password must equal the input's, and all four
/// session fields must be present. `token` selects the Nide or
/// Auth field family.
pub fn server_cached_session(store: &AccountStore, input: &McLoginServer) -> Option<ServerCache> {
    let (user_name, password, cache) = match input.token.as_str() {
        "Nide" => (
            &store.nide_user_name,
            &store.nide_password,
            ServerCache {
                access_token: store.nide_access.clone(),
                client_token: store.nide_client.clone(),
                uuid: store.nide_uuid.clone(),
                name: store.nide_name.clone(),
            },
        ),
        "Auth" => (
            &store.auth_user_name,
            &store.auth_password,
            ServerCache {
                access_token: store.auth_access.clone(),
                client_token: store.auth_client.clone(),
                uuid: store.auth_uuid.clone(),
                name: store.auth_name.clone(),
            },
        ),
        _ => return None,
    };
    if *user_name == input.user_name
        && *password == input.password
        && !cache.access_token.is_empty()
        && !cache.client_token.is_empty()
        && !cache.uuid.is_empty()
        && !cache.name.is_empty()
    {
        Some(cache)
    } else {
        None
    }
}

fn server_url(input: &McLoginServer, endpoint: &str) -> String {
    format!("{}{}", input.base_url.trim_end_matches('/'), endpoint)
}

fn server_result(input: &McLoginServer, cache: &ServerCache) -> McLoginResult {
    McLoginResult {
        name: cache.name.clone(),
        uuid: cache.uuid.clone(),
        access_token: cache.access_token.clone(),
        result_type: input.token.clone(),
        client_token: cache.client_token.clone(),
        profile_json: String::new(),
    }
}

/// `/validate` (`McLoginRequestValidate`): success is a 2xx answer
/// (upstream's client throws on error status, and PCL treats any
/// exception as "session invalid"). The result is the cached
/// session verbatim.
pub fn server_validate(
    http: &dyn HttpClient,
    input: &McLoginServer,
    cache: &ServerCache,
) -> Result<McLoginResult, LoginError> {
    let req = HttpRequest::post_json(
        &server_url(input, "/validate"),
        &json!({
            "accessToken": cache.access_token,
            "clientToken": cache.client_token,
            "requestUser": true,
        })
        .to_string(),
    );
    http.request(&req)
        .map_err(|e| msg(&format!("验证登录失败：{e}")))?;
    Ok(server_result(input, cache))
}

/// `/refresh` (`McLoginRequestRefresh`): refreshes the session;
/// `need_refresh_round` mirrors the upstream flag that adds the
/// selected profile after a fresh authenticate.
pub fn server_refresh(
    http: &dyn HttpClient,
    input: &McLoginServer,
    cache: &ServerCache,
) -> Result<ServerCache, LoginError> {
    let req = HttpRequest::post_json(
        &server_url(input, "/refresh"),
        &json!({
            "accessToken": cache.access_token,
            "clientToken": cache.client_token,
            "requestUser": true,
            "selectedProfile": { "id": cache.uuid, "name": cache.name },
        })
        .to_string(),
    );
    let resp = http
        .request(&req)
        .map_err(|e| msg(&format!("刷新登录失败：{e}")))?;
    let body = parse_json(&resp.text(), "刷新登录失败")?;
    if let Some(error) = body.get("error").and_then(Value::as_str) {
        let message = body
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or(error);
        return Err(msg(message));
    }
    let mut refreshed = cache.clone();
    refreshed.access_token = json_string(&body, "accessToken");
    if let Some(selected) = body.get("selectedProfile") {
        refreshed.uuid = json_string(selected, "id");
        refreshed.name = json_string(selected, "name");
    }
    if let Some(client_token) = body.get("clientToken").and_then(Value::as_str) {
        refreshed.client_token = client_token.to_string();
    }
    Ok(refreshed)
}

/// `/authenticate` (`McLoginRequestLogin`): note the request
/// carries no clientToken upstream — it comes from the response.
/// Returns the session plus the NeedRefresh flag: set when no
/// profile was pre-selected (or a reselect is forced) and more
/// than one profile exists; upstream then asks the user to pick
/// (preferring the cached UUID) and refreshes with the selection.
/// The port prefers the cached UUID like upstream and otherwise
/// takes the first profile, as a headless client must.
pub fn server_authenticate(
    http: &dyn HttpClient,
    input: &McLoginServer,
    cached_uuid: &str,
) -> Result<(ServerCache, bool), LoginError> {
    let req = HttpRequest::post_json(
        &server_url(input, "/authenticate"),
        &json!({
            "agent": { "name": "Minecraft", "version": 1 },
            "username": input.user_name,
            "password": input.password,
            "requestUser": true,
        })
        .to_string(),
    );
    let resp = http
        .request(&req)
        .map_err(|e| msg(&format!("登录失败：{e}")))?;
    let body = parse_json(&resp.text(), "登录失败")?;
    if let Some(error) = body.get("error").and_then(Value::as_str) {
        let message = body
            .get("errorMessage")
            .and_then(Value::as_str)
            .unwrap_or(error);
        return Err(msg(message));
    }
    let available = body
        .get("availableProfiles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if available.is_empty() {
        return Err(msg("你还没有创建角色，请在创建角色后再试！"));
    }
    let selected = body.get("selectedProfile").filter(|s| !s.is_null());
    let mut cache = ServerCache {
        access_token: json_string(&body, "accessToken"),
        client_token: json_string(&body, "clientToken"),
        uuid: String::new(),
        name: String::new(),
    };
    if (selected.is_none() || input.force_reselect_profile) && available.len() > 1 {
        let pick = available
            .iter()
            .find(|p| json_string(p, "id") == cached_uuid)
            .unwrap_or(&available[0]);
        cache.uuid = json_string(pick, "id");
        cache.name = json_string(pick, "name");
        Ok((cache, true))
    } else {
        let profile = selected.unwrap_or(&available[0]);
        cache.uuid = json_string(profile, "id");
        cache.name = json_string(profile, "name");
        Ok((cache, false))
    }
}

/// `McLoginServerStart`: validate the cached session when the
/// gate passes (and no forced profile reselect), else refresh,
/// else authenticate; a NeedRefresh authenticate is followed by
/// exactly one more refresh, whose failure is fatal
/// ("二轮刷新登录失败").
pub fn server_login(
    http: &dyn HttpClient,
    input: &McLoginServer,
    store: &AccountStore,
) -> Result<McLoginResult, LoginError> {
    let stored = server_cached_session(store, input);
    if !input.force_reselect_profile {
        if let Some(cache) = &stored {
            if let Ok(result) = server_validate(http, input, cache) {
                return Ok(result);
            }
            if let Ok(refreshed) = server_refresh(http, input, cache) {
                return Ok(server_result(input, &refreshed));
            }
        }
    }
    let cached_uuid = stored.map(|c| c.uuid).unwrap_or_default();
    let (cache, need_refresh) = server_authenticate(http, input, &cached_uuid)?;
    if need_refresh {
        let refreshed = server_refresh(http, input, &cache)
            .map_err(|e| msg(&format!("二轮刷新登录失败：{e}")))?;
        return Ok(server_result(input, &refreshed));
    }
    Ok(server_result(input, &cache))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{McLoginLegacy, McLoginMs, McLoginServer};
    use crate::http::FakeHttp;

    fn options() -> MsLoginOptions {
        MsLoginOptions {
            client_id: "test-client-id".to_string(),
            poll_interval: Duration::ZERO,
            max_poll_attempts: 10,
        }
    }

    fn xbl_xsts_mc_ent_profile() -> Vec<Result<crate::http::HttpResponse, crate::http::HttpError>> {
        vec![
            FakeHttp::ok_json(
                r#"{"Token":"xbl-token","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#,
            ),
            FakeHttp::ok_json(r#"{"Token":"xsts-token"}"#),
            FakeHttp::ok_json(r#"{"access_token":"mc-access","expires_in":86400}"#),
            FakeHttp::ok_json(r#"{"items":[{"name":"game_minecraft"}]}"#),
            FakeHttp::ok_json(
                r#"{"id":"uuid-1234","name":"Steve","skins":[{"state":"ACTIVE","url":"http://x/skin.png"}]}"#,
            ),
        ]
    }

    #[test]
    fn legacy_uuid_shape_and_skin_sex() {
        let uuid = legacy_uuid("Steve");
        assert_eq!(uuid.len(), 32);
        assert_eq!(&uuid[12..13], "3");
        assert_eq!(&uuid[16..17], "9");
        assert_eq!(uuid, legacy_uuid("Steve"), "deterministic");
        assert_ne!(uuid, legacy_uuid("Alex"));
        assert_eq!(skin_sex("short"), "Steve");
        // Type 0 keeps the raw UUID; result tokens mirror the UUID.
        let input = McLoginLegacy {
            user_name: "Steve".to_string(),
            ..McLoginLegacy::default()
        };
        let result = legacy_login(&input, false, false, &|_| {
            Err(msg("unused"))
        })
        .unwrap();
        assert_eq!(result.uuid, uuid);
        assert_eq!(result.access_token, uuid);
        assert_eq!(result.client_token, uuid);
        assert_eq!(result.result_type, "Legacy");
    }

    #[test]
    fn legacy_skin_type_bumping() {
        // Find one name of each natural skin sex, then verify the
        // opposite type bumps only the last 5 hex digits.
        let names = [
            "Steve", "Alex", "Bob", "Alice", "Eve", "Mallory", "Trent", "Peggy", "Walter",
            "Jesse",
        ];
        let alex_name = names
            .iter()
            .find(|n| skin_sex(&legacy_uuid(n)) == "Alex")
            .expect("some name is naturally Alex");
        let steve_name = names
            .iter()
            .find(|n| skin_sex(&legacy_uuid(n)) == "Steve")
            .expect("some name is naturally Steve");
        let to_steve = McLoginLegacy {
            user_name: alex_name.to_string(),
            skin_type: 1,
            ..McLoginLegacy::default()
        };
        let bumped = legacy_uuid_with_skin(&to_steve, false, false, &|_| Err(msg("unused")));
        assert_eq!(skin_sex(&bumped), "Steve");
        assert_eq!(&bumped[..27], &legacy_uuid(alex_name)[..27]);
        assert_ne!(bumped, legacy_uuid(alex_name));
        let to_alex = McLoginLegacy {
            user_name: steve_name.to_string(),
            skin_type: 2,
            ..McLoginLegacy::default()
        };
        let bumped = legacy_uuid_with_skin(&to_alex, false, false, &|_| Err(msg("unused")));
        assert_eq!(skin_sex(&bumped), "Alex");
        // Already-matching types stay put.
        let stays = McLoginLegacy {
            user_name: steve_name.to_string(),
            skin_type: 1,
            ..McLoginLegacy::default()
        };
        assert_eq!(
            legacy_uuid_with_skin(&stays, false, false, &|_| Err(msg("unused"))),
            legacy_uuid(steve_name)
        );
    }

    #[test]
    fn ms_device_code_full_chain() {
        let mut script = vec![
            FakeHttp::ok_json(
                r#"{"user_code":"ABCD-EFGH","verification_uri":"https://www.microsoft.com/link","device_code":"dc-1","expires_in":900,"interval":5}"#,
            ),
            FakeHttp::status(400, r#"{"error":"authorization_pending"}"#),
            FakeHttp::ok_json(r#"{"access_token":"oauth-access","refresh_token":"oauth-refresh"}"#),
        ];
        script.extend(xbl_xsts_mc_ent_profile());
        let http = FakeHttp::new(script);
        let store = AccountStore::default();
        let input = McLoginMs::default();
        let mut prompted: Option<DeviceCodePrompt> = None;
        let session = ms_login(&http, &options(), &input, &store, 1_000_000, &mut |p| {
            prompted = Some(p.clone());
        })
        .unwrap();
        let prompt = prompted.expect("device prompt shown");
        assert_eq!(prompt.user_code, "ABCD-EFGH");
        assert_eq!(session.result.name, "Steve");
        assert_eq!(session.result.uuid, "uuid-1234");
        assert_eq!(session.result.access_token, "mc-access");
        assert_eq!(session.result.client_token, "uuid-1234", "ClientToken is the UUID");
        assert_eq!(session.oauth_refresh_token, "oauth-refresh");
        assert_eq!(session.expires_unix, 1_000_000 + 86400 - 3600);
        let requests = http.requests.lock().unwrap();
        let urls: Vec<&str> = requests.iter().map(|r| r.url.as_str()).collect();
        assert!(urls[0].ends_with("/devicecode"));
        assert!(urls[1].ends_with("/token") && urls[2].ends_with("/token"));
        assert!(urls[3].contains("user.auth.xboxlive.com"));
        assert!(urls[4].contains("xsts.auth.xboxlive.com"));
        assert!(urls[5].contains("login_with_xbox"));
        assert!(urls[6].contains("entitlements/mcstore"));
        assert!(urls[7].contains("minecraft/profile"));
        // The XBL request must carry the d= prefixed RpsTicket and
        // the Minecraft login the XBL3.0 identity token.
        let xbl_body = String::from_utf8_lossy(requests[3].body.as_ref().unwrap()).into_owned();
        assert!(xbl_body.contains("\"RpsTicket\":\"d=oauth-access\""), "{xbl_body}");
        let mc_body = String::from_utf8_lossy(requests[5].body.as_ref().unwrap()).into_owned();
        assert!(mc_body.contains("XBL3.0 x=uhs-1;xsts-token"), "{mc_body}");
    }

    #[test]
    fn ms_cache_hit_skips_network() {
        let mut store = AccountStore::default();
        store.ms_expires = 2_000_000;
        store.ms_name = "Steve".to_string();
        store.ms_access = "cached-access".to_string();
        store.ms_uuid = "cached-uuid".to_string();
        store.ms_profile_json = "{}".to_string();
        let http = FakeHttp::new(vec![]);
        let input = McLoginMs {
            user_name: "Steve".to_string(),
            ..McLoginMs::default()
        };
        let session = ms_login(&http, &options(), &input, &store, 1_000_000, &mut |_| {
            panic!("no prompt on a cache hit")
        })
        .unwrap();
        assert_eq!(session.result.access_token, "cached-access");
        assert_eq!(session.result.client_token, "cached-uuid");
        assert!(http.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn ms_refresh_path_and_relogin_fallback() {
        // Happy refresh path.
        let mut script = vec![FakeHttp::ok_json(
            r#"{"access_token":"oauth-access-2","refresh_token":"oauth-refresh-2"}"#,
        )];
        script.extend(xbl_xsts_mc_ent_profile());
        let http = FakeHttp::new(script);
        let input = McLoginMs {
            oauth_refresh_token: "old-refresh".to_string(),
            ..McLoginMs::default()
        };
        let session = ms_login(
            &http,
            &options(),
            &input,
            &AccountStore::default(),
            1_000_000,
            &mut |_| panic!("no device prompt on the refresh path"),
        )
        .unwrap();
        assert_eq!(session.oauth_refresh_token, "oauth-refresh-2");
        assert!(http.requests.lock().unwrap()[0]
            .url
            .contains("login.live.com/oauth20_token.srf"));

        // invalid_grant → Relogin → device-code flow. The scripted
        // transport retries a 400 three times before surfacing it.
        let dead = || FakeHttp::status(400, r#"{"error":"invalid_grant"}"#);
        let mut script = vec![dead(), dead(), dead()];
        script.push(FakeHttp::ok_json(
            r#"{"user_code":"CODE","verification_uri":"https://www.microsoft.com/link","device_code":"dc"}"#,
        ));
        script.push(FakeHttp::ok_json(
            r#"{"access_token":"oauth-access","refresh_token":"oauth-refresh"}"#,
        ));
        script.extend(xbl_xsts_mc_ent_profile());
        let http = FakeHttp::new(script);
        let session = ms_login(
            &http,
            &options(),
            &input,
            &AccountStore::default(),
            1_000_000,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(session.result.name, "Steve");
    }

    #[test]
    fn ms_xsts_error_mapping_and_ownership_gate() {
        // XSTS XErr 2148916233 → the "no Xbox account" guidance.
        let xerr = || {
            FakeHttp::status(
                401,
                r#"{"XErr":2148916233,"Message":"no xbox account"}"#,
            )
        };
        let script = vec![
            FakeHttp::ok_json(r#"{"access_token":"oa","refresh_token":"or"}"#),
            FakeHttp::ok_json(
                r#"{"Token":"xbl","DisplayClaims":{"xui":[{"uhs":"u"}]}}"#,
            ),
            xerr(),
            xerr(),
            xerr(),
        ];
        let http = FakeHttp::new(script);
        let input = McLoginMs {
            oauth_refresh_token: "rt".to_string(),
            ..McLoginMs::default()
        };
        let err = ms_login(
            &http,
            &options(),
            &input,
            &AccountStore::default(),
            1_000_000,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("没有 Xbox Live 账户"), "{err}");

        // Empty entitlements → the not-purchased message.
        let mut script = vec![FakeHttp::ok_json(
            r#"{"access_token":"oa","refresh_token":"or"}"#,
        )];
        script.extend([
            FakeHttp::ok_json(
                r#"{"Token":"xbl","DisplayClaims":{"xui":[{"uhs":"u"}]}}"#,
            ),
            FakeHttp::ok_json(r#"{"Token":"xsts"}"#),
            FakeHttp::ok_json(r#"{"access_token":"mc","expires_in":3600}"#),
            FakeHttp::ok_json(r#"{"items":[]}"#),
        ]);
        let http = FakeHttp::new(script);
        let err = ms_login(
            &http,
            &options(),
            &input,
            &AccountStore::default(),
            1_000_000,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("未购买"), "{err}");
    }

    #[test]
    fn server_validate_uses_cached_session() {
        let mut store = AccountStore::default();
        store.nide_user_name = "user@example.com".to_string();
        store.nide_password = "secret".to_string();
        store.nide_access = "at-1".to_string();
        store.nide_client = "ct-1".to_string();
        store.nide_uuid = "uuid-1".to_string();
        store.nide_name = "Hero".to_string();
        let input = McLoginServer::nide("user@example.com", "secret", "server-42");
        let http = FakeHttp::new(vec![FakeHttp::ok_json("")]);
        let result = server_login(&http, &input, &store).unwrap();
        assert_eq!(result.name, "Hero");
        assert_eq!(result.access_token, "at-1");
        assert_eq!(result.result_type, "Nide");
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].url.ends_with("/authserver/validate"));
    }

    #[test]
    fn server_authenticate_then_refresh_selects_profile() {
        let input = McLoginServer::auth("user", "secret", "https://auth.example.com");
        let http = FakeHttp::new(vec![
            FakeHttp::ok_json(
                r#"{"accessToken":"at-1","clientToken":"ct-1","availableProfiles":[{"id":"id-1","name":"First"},{"id":"id-2","name":"Second"}]}"#,
            ),
            FakeHttp::ok_json(
                r#"{"accessToken":"at-2","clientToken":"ct-1","selectedProfile":{"id":"id-1","name":"First"}}"#,
            ),
        ]);
        let result = server_login(&http, &input, &AccountStore::default()).unwrap();
        assert_eq!(result.access_token, "at-2", "the follow-up refresh wins");
        assert_eq!(result.name, "First");
        assert_eq!(result.result_type, "Auth");
        let requests = http.requests.lock().unwrap();
        assert!(requests[0].url.ends_with("/authserver/authenticate"));
        assert!(requests[1].url.ends_with("/authserver/refresh"));
        // The authenticate request must not carry a clientToken.
        let body = String::from_utf8_lossy(requests[0].body.as_ref().unwrap()).into_owned();
        assert!(!body.contains("clientToken"), "{body}");
    }
}
