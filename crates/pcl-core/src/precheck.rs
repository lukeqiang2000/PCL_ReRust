//! The launch precheck — port of `ModLaunch.McLaunchPrecheck`,
//! including the license-mandated persuasion checkpoints.
//!
//! Upstream order:
//! 1. Path check: the indie path and the version path must not
//!    contain `!` or `;`.
//! 2. Instance check: a selected instance whose state is Error
//!    blocks the launch with its info text.
//! 3. Login check: `McLoginAble` over the current login input
//!    (the per-type `IsVaild` checks ported in M1).
//! 4. Donation prompt (Release builds only, skipped when saving a
//!    launch script): at `SystemLaunchCount` ∈ {10, 20, 40, …,
//!    2000} the user is asked to sponsor PCL on 爱发电.
//! 5. Genuine-game prompt (skipped when saving a script): when
//!    `HintBuy` is not set and the login type is not Microsoft —
//!    Chinese locales get the purchase suggestion at launch counts
//!    {3, 8, 15, …, 1900}; other locales with Legacy login get a
//!    blocking gate (buy / play the demo with `--demo` / cancel).
//!
//! The prompts are modeled as data: [`precheck`] returns the hard
//! failures as errors and the prompts to show as values, so the
//! CLI (or later the GUI) decides how to present them. The
//! demo-gate decision feeds back through [`DemoGateChoice`].

use crate::account::{McLoginData, McLoginType};

/// `SystemLaunchCount` values that trigger the donation prompt.
pub const DONATE_COUNTS: &[i64] = &[
    10, 20, 40, 60, 80, 100, 120, 150, 200, 250, 300, 350, 400, 500, 600, 700, 800, 900, 1000,
    1200, 1400, 1600, 1800, 2000,
];

/// `SystemLaunchCount` values that trigger the purchase prompt.
pub const BUY_COUNTS: &[i64] = &[
    3, 8, 15, 30, 50, 70, 90, 110, 130, 180, 220, 280, 330, 380, 450, 550, 660, 750, 880, 950,
    1100, 1300, 1500, 1700, 1900,
];

/// The 爱发电 sponsor page the donation prompt opens.
pub const DONATE_URL: &str = "https://meloong.com/afd/a/LTCat";
/// The Microsoft Store page the purchase prompt opens.
pub const BUY_URL: &str =
    "https://www.xbox.com/zh-cn/games/store/minecraft-java-bedrock-edition-for-pc/9nxp44l49shj";

/// Inputs to [`precheck`], mirroring the settings and state
/// `McLaunchPrecheck` reads.
#[derive(Debug, Clone)]
pub struct PrecheckInput {
    /// `McInstanceSelected.PathIndie`.
    pub indie_path: String,
    /// `McInstanceSelected.PathVersion`.
    pub version_path: String,
    /// The selected instance's error info when its state is Error.
    pub instance_error: Option<String>,
    /// The current login input (`McLoginInput()`), for `McLoginAble`.
    pub login: McLoginData,
    /// The active login type (`LoginType` setting).
    pub login_type: McLoginType,
    /// `HintBuy`: a genuine login has happened before.
    pub hint_buy: bool,
    /// `SystemLaunchCount`.
    pub launch_count: i64,
    /// Whether this is a Release build (`BuildType = Release`);
    /// the donation prompt exists only there.
    pub is_release_build: bool,
    /// Saving a launch script instead of launching
    /// (`CurrentLaunchOptions.SaveBatch`): both prompts are
    /// skipped.
    pub save_batch: bool,
    /// Whether the UI culture is Chinese (the purchase prompt's
    /// two shapes are locale-selected upstream).
    pub is_chinese_locale: bool,
}

/// A non-blocking persuasion prompt to present (upstream shows it
/// on a side thread; the game launches regardless of the answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrecheckPrompt {
    /// Ask for a PCL sponsorship (opens [`DONATE_URL`] on accept).
    Donate,
    /// Suggest buying the genuine game (opens [`BUY_URL`] on accept).
    Buy,
}

/// The blocking non-Chinese Legacy gate's possible answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoGateChoice {
    /// "购买正版": open the store page; the launch continues.
    Buy,
    /// "试玩": launch in demo mode (`--demo` is added).
    Demo,
    /// "返回": cancel the launch.
    Cancel,
}

/// Outcome of [`precheck`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrecheckOutcome {
    /// Launch may proceed; show these prompts alongside.
    Proceed { prompts: Vec<PrecheckPrompt> },
    /// The non-Chinese Legacy gate must be answered first. Call
    /// [`PrecheckOutcome::after_demo_gate`] with the choice.
    DemoGate,
}

impl PrecheckOutcome {
    /// Resolve a [`PrecheckOutcome::DemoGate`] with the user's
    /// choice: Demo adds `--demo` (reported via the return value),
    /// Cancel aborts the launch.
    pub fn after_demo_gate(self, choice: DemoGateChoice) -> Result<bool, String> {
        match choice {
            DemoGateChoice::Buy => Ok(false),
            DemoGateChoice::Demo => Ok(true),
            DemoGateChoice::Cancel => Err("登录已取消".to_string()),
        }
    }
}

/// Run the precheck. Hard failures (bad path, broken instance,
/// invalid login input) are `Err` with upstream's message text.
pub fn precheck(input: &PrecheckInput) -> Result<PrecheckOutcome, String> {
    // 1. Path check.
    if input.indie_path.contains('!') || input.indie_path.contains(';') {
        return Err(format!(
            "游戏路径中不可包含 ! 或 ;（{}）",
            input.indie_path
        ));
    }
    if input.version_path.contains('!') || input.version_path.contains(';') {
        return Err(format!(
            "游戏路径中不可包含 ! 或 ;（{}）",
            input.version_path
        ));
    }
    // 2. Instance check.
    if let Some(info) = &input.instance_error {
        return Err(format!("Minecraft 存在问题：{info}"));
    }
    // 3. Login check (McLoginAble over the per-type IsVaild).
    let login_check = match &input.login {
        McLoginData::Ms(data) => data.validate(),
        McLoginData::Legacy(data) => data.validate(),
        McLoginData::Server(data) => data.validate(),
    };
    if let Err(message) = login_check {
        return Err(message.to_string());
    }
    // 4 + 5. Persuasion checkpoints (skipped for script saving).
    let mut prompts = Vec::new();
    if !input.save_batch {
        if input.is_release_build && DONATE_COUNTS.contains(&input.launch_count) {
            prompts.push(PrecheckPrompt::Donate);
        }
        if !input.hint_buy && input.login_type != McLoginType::Ms {
            if input.is_chinese_locale {
                if BUY_COUNTS.contains(&input.launch_count) {
                    prompts.push(PrecheckPrompt::Buy);
                }
            } else if input.login_type == McLoginType::Legacy {
                return Ok(PrecheckOutcome::DemoGate);
            }
        }
    }
    Ok(PrecheckOutcome::Proceed { prompts })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{McLoginLegacy, McLoginMs};

    fn legacy_input(count: i64, chinese: bool) -> PrecheckInput {
        PrecheckInput {
            indie_path: "C:\\MC\\versions\\1.20.1".to_string(),
            version_path: "C:\\MC\\versions\\1.20.1".to_string(),
            instance_error: None,
            login: McLoginData::Legacy(McLoginLegacy {
                user_name: "Steve".to_string(),
                ..McLoginLegacy::default()
            }),
            login_type: McLoginType::Legacy,
            hint_buy: false,
            launch_count: count,
            is_release_build: true,
            save_batch: false,
            is_chinese_locale: chinese,
        }
    }

    #[test]
    fn bad_paths_and_broken_instance_block() {
        let mut input = legacy_input(1, true);
        input.indie_path = "C:\\MC;\\versions".to_string();
        assert!(precheck(&input).unwrap_err().contains("不可包含"));
        let mut input = legacy_input(1, true);
        input.instance_error = Some("JSON 损坏".to_string());
        assert!(precheck(&input).unwrap_err().contains("Minecraft 存在问题"));
    }

    #[test]
    fn invalid_login_blocks() {
        let mut input = legacy_input(1, true);
        input.login = McLoginData::Legacy(McLoginLegacy::default());
        assert!(precheck(&input).is_err(), "empty offline name is invalid");
        input.login = McLoginData::Ms(McLoginMs::default());
        input.login_type = McLoginType::Ms;
        assert!(precheck(&input).is_err(), "MS login without a refresh token is invalid");
    }

    #[test]
    fn donate_and_buy_prompts_at_their_counts() {
        // Count 10: donate fires; buy counts do not include 10.
        match precheck(&legacy_input(10, true)).unwrap() {
            PrecheckOutcome::Proceed { prompts } => {
                assert_eq!(prompts, vec![PrecheckPrompt::Donate]);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
        // Count 3 (Chinese): buy fires, donate does not.
        match precheck(&legacy_input(3, true)).unwrap() {
            PrecheckOutcome::Proceed { prompts } => {
                assert_eq!(prompts, vec![PrecheckPrompt::Buy]);
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
        // Ordinary count: nothing.
        match precheck(&legacy_input(7, true)).unwrap() {
            PrecheckOutcome::Proceed { prompts } => assert!(prompts.is_empty()),
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn prompts_suppressed_for_ms_hint_and_batch() {
        // A Microsoft login never sees the buy prompt (and count 3
        // is not a donate count, isolating the buy rule).
        let mut input = legacy_input(3, true);
        input.login_type = McLoginType::Ms;
        input.login = McLoginData::Ms(McLoginMs {
            oauth_refresh_token: "rt".to_string(),
            ..McLoginMs::default()
        });
        match precheck(&input).unwrap() {
            PrecheckOutcome::Proceed { prompts } => assert!(prompts.is_empty()),
            other => panic!("expected Proceed, got {other:?}"),
        }
        // HintBuy set: no buy prompt.
        let mut input = legacy_input(3, true);
        input.hint_buy = true;
        match precheck(&input).unwrap() {
            PrecheckOutcome::Proceed { prompts } => assert!(prompts.is_empty()),
            other => panic!("expected Proceed, got {other:?}"),
        }
        // Saving a script: neither prompt even at a donate count.
        let mut input = legacy_input(10, true);
        input.save_batch = true;
        match precheck(&input).unwrap() {
            PrecheckOutcome::Proceed { prompts } => assert!(prompts.is_empty()),
            other => panic!("expected Proceed, got {other:?}"),
        }
        // Debug builds: no donate prompt.
        let mut input = legacy_input(10, true);
        input.is_release_build = false;
        match precheck(&input).unwrap() {
            PrecheckOutcome::Proceed { prompts } => assert!(prompts.is_empty()),
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn non_chinese_legacy_hits_the_demo_gate() {
        match precheck(&legacy_input(1, false)).unwrap() {
            PrecheckOutcome::DemoGate => {}
            other => panic!("expected DemoGate, got {other:?}"),
        }
        assert_eq!(
            PrecheckOutcome::DemoGate.after_demo_gate(DemoGateChoice::Demo),
            Ok(true)
        );
        assert_eq!(
            PrecheckOutcome::DemoGate.after_demo_gate(DemoGateChoice::Buy),
            Ok(false)
        );
        assert!(PrecheckOutcome::DemoGate
            .after_demo_gate(DemoGateChoice::Cancel)
            .is_err());
    }
}
