use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

const APP: &str = "magicbot";
const BOT_NAME: &str = "magicbot";
const STATE_DIR: &str = "/var/lib/magicbot";
const RUN_DIR: &str = "/run/magicbot";
const LOG_DIR: &str = "/var/log/magicbot";
const SYSTEMD_UNIT: &str = "/etc/systemd/system/magicbot.service";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GlobalConfig {
	installed_at: i64,
	account: Option<String>,
	signal_cli_config_dir: Option<String>,
	selected_group: Option<String>,
	daemon_enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct GroupConfig {
	group_id: String,
	group_name: String,
	enabled: bool,
	only_admin_can_ban: bool,
	require_bot_admin_to_enforce: bool,

	welcome_template: Option<String>,

	auto_replies: Vec<KeywordGroupReply>,
	warn_rules: Vec<KeywordGroupWarn>,
	ban_rules: Vec<KeywordGroupBan>,

	warn_window_minutes: u64,
	warn_max_count: u32,
	warn_message: String,

	desired_permission_add_member: String,
	desired_permission_send_message: String,
	desired_permission_edit_details: String,

	last_members_snapshot: BTreeSet<String>,
	bot_has_admin: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KeywordGroupReply {
	keywords: Vec<String>,
	reply: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KeywordGroupWarn {
	keywords: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct KeywordGroupBan {
	keywords: Vec<String>,
}

#[derive(Clone, Debug)]
struct GroupRuntime {
	cfg: GroupConfig,
	admins: BTreeSet<String>,
	members: BTreeSet<String>,
	member_names: HashMap<String, String>,
	self_id: String,
}

#[derive(Clone, Debug)]
struct Identity {
	id: String,
	number: Option<String>,
	name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReceiveEnvelope {
	envelope: EnvelopeInner,
	account: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct EnvelopeInner {
	source: Option<String>,
	#[serde(rename = "sourceNumber")]
	source_number: Option<String>,
	#[serde(rename = "sourceUuid")]
	source_uuid: Option<String>,
	#[serde(rename = "sourceName")]
	source_name: Option<String>,
	timestamp: Option<i64>,
	#[serde(rename = "dataMessage")]
	data_message: Option<DataMessage>,
}

#[derive(Clone, Debug, Deserialize)]
struct DataMessage {
	message: Option<String>,
	#[serde(rename = "groupInfo")]
	group_info: Option<GroupInfo>,
	quote: Option<Quote>,
	#[serde(rename = "expiresInSeconds")]
	expires_in_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct Quote {
	author: Option<String>,
	#[serde(rename = "id")]
	id: Option<i64>,
	text: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct GroupInfo {
	#[serde(rename = "groupId")]
	group_id: String,
	#[serde(rename = "groupName")]
	group_name: Option<String>,
	revision: Option<u64>,
	#[serde(rename = "type")]
	kind: String,
}

fn main() {
	if let Err(e) = real_main() {
		eprintln!("[ERR] {e:#}");
		std::process::exit(1);
	}
}

fn real_main() -> Result<()> {
	ensure_dirs()?;
	let args: Vec<String> = env::args().collect();
	if args.len() >= 2 && args[1] == "--daemon" {
		let gc = load_global()?;
		let acc = gc
			.account
			.clone()
			.ok_or_else(|| anyhow!("No account set. Please run `magicbot` and login first."))?;
		run_daemon(&acc)?;
		return Ok(());
	}
	show_menu()?;
	Ok(())
}

fn ensure_dirs() -> Result<()> {
	for d in [STATE_DIR, RUN_DIR, LOG_DIR] {
		if !Path::new(d).exists() {
			fs::create_dir_all(d).with_context(|| format!("create dir {d}"))?;
		}
	}
	Ok(())
}

fn global_path() -> PathBuf {
	PathBuf::from(STATE_DIR).join("global.json")
}

fn groups_dir() -> PathBuf {
	PathBuf::from(STATE_DIR).join("groups")
}

fn group_cfg_path(gid: &str) -> PathBuf {
	groups_dir().join(format!("{gid}.json"))
}

fn group_mark_dir(gid: &str) -> PathBuf {
	PathBuf::from(STATE_DIR).join("marks").join(gid)
}

fn load_global() -> Result<GlobalConfig> {
	let p = global_path();
	if !p.exists() {
		let gc = GlobalConfig {
			installed_at: Utc::now().timestamp(),
			account: None,
			signal_cli_config_dir: None,
			selected_group: None,
			daemon_enabled: false,
		};
		save_global(&gc)?;
		return Ok(gc);
	}
	let mut s = String::new();
	File::open(&p)?.read_to_string(&mut s)?;
	let gc: GlobalConfig = serde_json::from_str(&s)?;
	Ok(gc)
}

fn save_global(gc: &GlobalConfig) -> Result<()> {
	let p = global_path();
	let tmp = p.with_extension("json.tmp");
	fs::write(&tmp, serde_json::to_vec_pretty(gc)?)?;
	fs::rename(tmp, p)?;
	Ok(())
}

fn load_group_cfg(gid: &str) -> Result<GroupConfig> {
	let p = group_cfg_path(gid);
	if !p.exists() {
		return Ok(GroupConfig {
			group_id: gid.to_string(),
			group_name: String::new(),
			enabled: false,
			only_admin_can_ban: true,
			require_bot_admin_to_enforce: true,
			welcome_template: None,
			auto_replies: vec![],
			warn_rules: vec![],
			ban_rules: vec![],
			warn_window_minutes: 10,
			warn_max_count: 3,
			warn_message: "警告：请停止违规内容，否则将被移出群组。".to_string(),
			desired_permission_add_member: "EVERY_MEMBER".to_string(),
			desired_permission_send_message: "EVERY_MEMBER".to_string(),
			desired_permission_edit_details: "ONLY_ADMINS".to_string(),
			last_members_snapshot: BTreeSet::new(),
			bot_has_admin: false,
		});
	}
	let mut s = String::new();
	File::open(&p)?.read_to_string(&mut s)?;
	let cfg: GroupConfig = serde_json::from_str(&s)?;
	Ok(cfg)
}

fn save_group_cfg(cfg: &GroupConfig) -> Result<()> {
	fs::create_dir_all(groups_dir())?;
	let p = group_cfg_path(&cfg.group_id);
	let tmp = p.with_extension("json.tmp");
	fs::write(&tmp, serde_json::to_vec_pretty(cfg)?)?;
	fs::rename(tmp, p)?;
	Ok(())
}

fn theme() -> ColorfulTheme {
	ColorfulTheme::default()
}

fn show_menu() -> Result<()> {
	let mut gc = load_global()?;
	loop {
		let acc = gc.account.clone().unwrap_or_else(|| "(未登录)".to_string());
		let title = format!(
			"╔════════════════════════════════════════╗\n\
		                     │          MagicBot (Signal)           │\n\
		                     ║  账号: {acc:<29}║\n\
		                     ╚════════════════════════════════════════╝"
		);
		println!("\n{title}\n");

		let items = vec![
			"1. 安装依赖(仅RHEL/Fedora): qrencode / curl / jq (可选)",
			"2. 登录/绑定设备(生成二维码)",
			"3. SMS注册/验证(可选)",
			"4. 选择群组 + 初始化配置",
			"5. 群组策略设置(关键词/欢迎语/权限/开关)",
			"6. 运行守护(前台测试)",
			"7. systemd 开机自启: 安装/启用/禁用/卸载",
			"8. 验证人类/解除限制(Captcha token)",
			"9. 退出登录并清理数据(本机)",
			"0. 退出",
		];

		let sel = Select::with_theme(&theme())
			.items(&items)
			.default(0)
			.interact()?;

		match sel {
			0 => install_deps()?,
			1 => login_linkdevice(&mut gc)?,
			2 => register_sms_flow(&mut gc)?,
			3 => select_group(&mut gc)?,
			4 => group_settings_menu(&mut gc)?,
			5 => run_daemon_front(&gc)?,
			6 => systemd_menu(&mut gc)?,
			7 => captcha_menu(&gc)?,
			8 => logout_and_cleanup(&mut gc)?,
			9 => break,
			_ => {}
		}
	}
	Ok(())
}

fn install_deps() -> Result<()> {
	require_root()?;
	let release = read_os_id()?;
	if release != "rhel" && release != "fedora" && release != "rocky" && release != "almalinux" {
		return Err(anyhow!("Only supports RHEL/Fedora family. Detected: {release}"));
	}

	println!("[INF] Installing packages via dnf ...");
	run_ok(Command::new("dnf").arg("-y").arg("install").arg("qrencode").arg("curl").arg("jq"))?;
	println!("[OK] Done.");
	Ok(())
}

fn login_linkdevice(gc: &mut GlobalConfig) -> Result<()> {
	require_root()?;
	ensure_cmd("signal-cli")?;
	ensure_cmd("qrencode")?;

	let cfgdir = Input::<String>::with_theme(&theme())
		.with_prompt("signal-cli --config 目录(留空用默认)")
		.allow_empty(true)
		.interact_text()?;
	let cfgdir = if cfgdir.trim().is_empty() { None } else { Some(cfgdir) };
	gc.signal_cli_config_dir = cfgdir;

	println!("\n[INF] 使用 linkdevice 绑定(推荐：绑定到手机/主设备)\n");

	let name = Input::<String>::with_theme(&theme())
		.with_prompt("设备名称(默认 magicbot)")
		.allow_empty(true)
		.interact_text()?;
	let name = if name.trim().is_empty() { "magicbot".to_string() } else { name };

	let mut cmd = Command::new("signal-cli");
	if let Some(dir) = &gc.signal_cli_config_dir {
		cmd.arg("--config").arg(dir);
	}
	cmd.arg("link").arg("-n").arg(name);

	let out = cmd.output().context("signal-cli link failed")?;
	let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
	if s.is_empty() {
		return Err(anyhow!(
			"signal-cli link returned empty output. stderr={}",
			String::from_utf8_lossy(&out.stderr)
		));
	}

	println!("\n[OK] Link URI:\n{s}\n");
	println!("[INF] QRCode(ANSI):\n");
	run_ok(Command::new("qrencode").arg("-t").arg("ANSIUTF8").arg(&s))?;

	println!("\n[INF] 绑定完成后，需要在本机选择一个账号进行后续操作。");
	println!("[INF] 如果你是“链接设备”，账号由主设备决定；一般无需输入手机号。");
	println!("[INF] 现在尝试自动发现本机已有账号...\n");

	let accs = list_local_accounts(gc)?;
	if accs.is_empty() {
		println!("[WRN] 没发现账号。你可能还没在手机端完成绑定，或 config 目录不对。");
		return Ok(());
	}

	let i = Select::with_theme(&theme())
		.with_prompt("选择要使用的账号")
		.items(&accs)
		.default(0)
		.interact()?;
	gc.account = Some(accs[i].clone());
	save_global(gc)?;
	println!("[OK] 当前账号 = {}", accs[i]);
	Ok(())
}

fn register_sms_flow(gc: &mut GlobalConfig) -> Result<()> {
	require_root()?;
	ensure_cmd("signal-cli")?;

	let phone = Input::<String>::with_theme(&theme())
		.with_prompt("手机号(含国家码，例 +1548xxxx)")
		.interact_text()?;

	let voice = Confirm::with_theme(&theme())
		.with_prompt("使用语音验证(voice)? 否则短信")
		.default(false)
		.interact()?;

	let captcha = Input::<String>::with_theme(&theme())
		.with_prompt("如遇 captcha required，可粘贴 signalcaptcha://... (留空跳过)")
		.allow_empty(true)
		.interact_text()?;
	let captcha = captcha.trim().to_string();

	let mut reg = Command::new("signal-cli");
	if let Some(dir) = &gc.signal_cli_config_dir {
		reg.arg("--config").arg(dir);
	}
	reg.arg("-a").arg(&phone).arg("register");
	if voice {
		reg.arg("--voice");
	}
	if !captcha.is_empty() {
		reg.arg("--captcha").arg(captcha);
	}
	run_ok(&mut reg)?;

	let code = Input::<String>::with_theme(&theme())
		.with_prompt("输入收到的验证码")
		.interact_text()?;

	let pin = Input::<String>::with_theme(&theme())
		.with_prompt("如设置过注册锁PIN则输入(留空跳过)")
		.allow_empty(true)
		.interact_text()?;

	let mut ver = Command::new("signal-cli");
	if let Some(dir) = &gc.signal_cli_config_dir {
		ver.arg("--config").arg(dir);
	}
	ver.arg("-a").arg(&phone).arg("verify").arg(code);
	if !pin.trim().is_empty() {
		ver.arg("--pin").arg(pin.trim());
	}
	run_ok(&mut ver)?;

	gc.account = Some(phone);
	save_global(gc)?;
	println!("[OK] 注册完成。");
	Ok(())
}

fn select_group(gc: &mut GlobalConfig) -> Result<()> {
	let acc = gc.account.clone().ok_or_else(|| anyhow!("未登录"))?;
	let groups = list_groups(&acc, gc.signal_cli_config_dir.as_deref())?;
	if groups.is_empty() {
		return Err(anyhow!("No groups found. 先确保该账号已加入群。"));
	}

	let mut names = vec![];
	for g in &groups {
		names.push(format!("{}  ({})", g.name, g.id));
	}
	let idx = Select::with_theme(&theme())
		.with_prompt("选择一个群组用于配置")
		.items(&names)
		.default(0)
		.interact()?;
	let gid = groups[idx].id.clone();

	let mut cfg = load_group_cfg(&gid)?;
	cfg.group_name = groups[idx].name.clone();
	if cfg.group_id.is_empty() {
		cfg.group_id = gid.clone();
	}
	save_group_cfg(&cfg)?;

	gc.selected_group = Some(gid.clone());
	save_global(gc)?;
	println!("[OK] 已选择群: {} ({})", cfg.group_name, gid);
	Ok(())
}

fn group_settings_menu(gc: &mut GlobalConfig) -> Result<()> {
	let _acc = gc.account.clone().ok_or_else(|| anyhow!("未登录"))?;
	let gid = gc
		.selected_group
		.clone()
		.ok_or_else(|| anyhow!("未选择群组"))?;

	let mut cfg = load_group_cfg(&gid)?;
	loop {
		println!("\n╔════════════════════════════════════════╗");
		println!("║ 群组: {:<34}║", truncate(&cfg.group_name, 34));
		println!("║ ID : {:<34}║", truncate(&cfg.group_id, 34));
		println!("╚════════════════════════════════════════╝");

		let items = vec![
			format!("1. 开关: {}", if cfg.enabled { "启用" } else { "关闭" }),
			format!(
				"2. 执行需要 Bot 管理员权限: {}",
				if cfg.require_bot_admin_to_enforce { "是" } else { "否" }
			),
			format!(
				"3. /ban 仅允许管理员触发: {}",
				if cfg.only_admin_can_ban { "是" } else { "否" }
			),
			"4. 欢迎语设置".to_string(),
			"5. 自动回复(增加/删除/清空)".to_string(),
			"6. 警告词(增加/删除/清空)".to_string(),
			"7. 违规词(增加/删除/清空)".to_string(),
			"8. 警告策略(次数/窗口/警告文案)".to_string(),
			"9. 接管策略: 当 Bot 被设为管理员后自动设置群权限".to_string(),
			"10. 返回".to_string(),
		];

		let idx = Select::with_theme(&theme())
			.items(&items)
			.default(0)
			.interact()?;

		match idx {
			0 => {
				cfg.enabled = !cfg.enabled;
				save_group_cfg(&cfg)?;
			}
			1 => {
				cfg.require_bot_admin_to_enforce = !cfg.require_bot_admin_to_enforce;
				save_group_cfg(&cfg)?;
			}
			2 => {
				cfg.only_admin_can_ban = !cfg.only_admin_can_ban;
				save_group_cfg(&cfg)?;
			}
			3 => {
				let tpl = Input::<String>::with_theme(&theme())
					.with_prompt("欢迎语模板(例：你好##{@user}##，欢迎进入RPM俱乐部。 留空=禁用)")
					.allow_empty(true)
					.interact_text()?;
				if tpl.trim().is_empty() {
					cfg.welcome_template = None;
				} else {
					cfg.welcome_template = Some(tpl);
				}
				save_group_cfg(&cfg)?;
			}
			4 => {
				cfg.auto_replies = keyword_group_reply_edit(cfg.auto_replies)?;
				save_group_cfg(&cfg)?;
			}
			5 => {
				cfg.warn_rules = keyword_group_simple_edit_warn(cfg.warn_rules)?;
				save_group_cfg(&cfg)?;
			}
			6 => {
				cfg.ban_rules = keyword_group_simple_edit_ban(cfg.ban_rules)?;
				save_group_cfg(&cfg)?;
			}
			7 => {
				let w = Input::<u64>::with_theme(&theme())
					.with_prompt("警告窗口(分钟)")
					.default(cfg.warn_window_minutes)
					.interact_text()?;
				let c = Input::<u32>::with_theme(&theme())
					.with_prompt("窗口内允许警告次数")
					.default(cfg.warn_max_count)
					.interact_text()?;
				let msg = Input::<String>::with_theme(&theme())
					.with_prompt("警告文案(发送给触发者)")
					.default(cfg.warn_message.clone())
					.interact_text()?;
				cfg.warn_window_minutes = w;
				cfg.warn_max_count = c;
				cfg.warn_message = msg;
				save_group_cfg(&cfg)?;
			}
			8 => {
				let add = Input::<String>::with_theme(&theme())
					.with_prompt("permissionAddMember: EVERY_MEMBER / ONLY_ADMINS")
					.default(cfg.desired_permission_add_member.clone())
					.interact_text()?;
				let send = Input::<String>::with_theme(&theme())
					.with_prompt("permissionSendMessage: EVERY_MEMBER / ONLY_ADMINS")
					.default(cfg.desired_permission_send_message.clone())
					.interact_text()?;
				let edit = Input::<String>::with_theme(&theme())
					.with_prompt("permissionEditDetails: EVERY_MEMBER / ONLY_ADMINS")
					.default(cfg.desired_permission_edit_details.clone())
					.interact_text()?;
				cfg.desired_permission_add_member = normalize_perm(&add);
				cfg.desired_permission_send_message = normalize_perm(&send);
				cfg.desired_permission_edit_details = normalize_perm(&edit);
				save_group_cfg(&cfg)?;
			}
			9 => break,
			_ => {}
		}
	}

	println!("[OK] 配置已保存。");
	println!("[INF] 守护进程会在检测到 Bot 获得管理员权限时自动接管并设置群权限。");
	println!("[INF] 现在可选：从主菜单启动守护(前台测试) 或 systemd 开机自启。");
	Ok(())
}

fn keyword_group_reply_edit(mut cur: Vec<KeywordGroupReply>) -> Result<Vec<KeywordGroupReply>> {
	loop {
		let mut items = vec![
			"增加".to_string(),
			"删除".to_string(),
			"删除全部".to_string(),
			"返回".to_string(),
		];

		let idx = Select::with_theme(&theme())
			.with_prompt("自动回复设置")
			.items(&items)
			.default(0)
			.interact()?;

		match idx {
			0 => {
				let (keywords, reply) = prompt_keyword_group_and_reply()?;
				cur.push(KeywordGroupReply { keywords, reply });
			}
			1 => {
				if cur.is_empty() {
					println!("[WRN] 为空。");
					continue;
				}
				let list = list_reply_groups(&cur);
				let d = Select::with_theme(&theme())
					.with_prompt("选择要删除的条目")
					.items(&list)
					.default(0)
					.interact()?;
				if Confirm::with_theme(&theme())
					.with_prompt("确认删除该条目？")
					.default(false)
					.interact()?
				{
					cur.remove(d);
				}
			}
			2 => {
				if Confirm::with_theme(&theme())
					.with_prompt("确认删除全部自动回复？")
					.default(false)
					.interact()?
				{
					cur.clear();
				}
			}
			3 => break,
			_ => {}
		}
	}
	Ok(cur)
}

fn keyword_group_simple_edit_warn(mut cur: Vec<KeywordGroupWarn>) -> Result<Vec<KeywordGroupWarn>> {
	loop {
		let idx = Select::with_theme(&theme())
			.with_prompt("警告词设置")
			.items(&["增加", "删除", "删除全部", "返回"])
			.default(0)
			.interact()?;
		match idx {
			0 => {
				let keywords = prompt_keyword_group_only()?;
				cur.push(KeywordGroupWarn { keywords });
			}
			1 => {
				if cur.is_empty() {
					println!("[WRN] 为空。");
					continue;
				}
				let list = cur
					.iter()
					.enumerate()
					.map(|(i, x)| format!("{}. {}", (b'A' + (i as u8)) as char, x.keywords.join(", ")))
					.collect::<Vec<_>>();
				let d = Select::with_theme(&theme())
					.with_prompt("选择要删除的条目")
					.items(&list)
					.default(0)
					.interact()?;
				if Confirm::with_theme(&theme())
					.with_prompt("确认删除？")
					.default(false)
					.interact()?
				{
					cur.remove(d);
				}
			}
			2 => {
				if Confirm::with_theme(&theme())
					.with_prompt("确认删除全部警告词？")
					.default(false)
					.interact()?
				{
					cur.clear();
				}
			}
			3 => break,
			_ => {}
		}
	}
	Ok(cur)
}

fn keyword_group_simple_edit_ban(mut cur: Vec<KeywordGroupBan>) -> Result<Vec<KeywordGroupBan>> {
	loop {
		let idx = Select::with_theme(&theme())
			.with_prompt("违规词设置(触发直接踢)")
			.items(&["增加", "删除", "删除全部", "返回"])
			.default(0)
			.interact()?;
		match idx {
			0 => {
				let keywords = prompt_keyword_group_only()?;
				cur.push(KeywordGroupBan { keywords });
			}
			1 => {
				if cur.is_empty() {
					println!("[WRN] 为空。");
					continue;
				}
				let list = cur
					.iter()
					.enumerate()
					.map(|(i, x)| format!("{}. {}", (b'A' + (i as u8)) as char, x.keywords.join(", ")))
					.collect::<Vec<_>>();
				let d = Select::with_theme(&theme())
					.with_prompt("选择要删除的条目")
					.items(&list)
					.default(0)
					.interact()?;
				if Confirm::with_theme(&theme())
					.with_prompt("确认删除？")
					.default(false)
					.interact()?
				{
					cur.remove(d);
				}
			}
			2 => {
				if Confirm::with_theme(&theme())
					.with_prompt("确认删除全部违规词？")
					.default(false)
					.interact()?
				{
					cur.clear();
				}
			}
			3 => break,
			_ => {}
		}
	}
	Ok(cur)
}

fn prompt_keyword_group_only() -> Result<Vec<String>> {
	let mut keywords = vec![];
	let first = Input::<String>::with_theme(&theme())
		.with_prompt("输入关键词(第1个)")
		.interact_text()?;
	keywords.push(first);

	loop {
		let add_more = Confirm::with_theme(&theme())
			.with_prompt("是否继续增加同组关键词？")
			.default(false)
			.interact()?;
		if !add_more {
			break;
		}
		let kw = Input::<String>::with_theme(&theme())
			.with_prompt("再输入一个关键词")
			.interact_text()?;
		keywords.push(kw);
	}
	Ok(keywords
		.into_iter()
		.map(|s| s.trim().to_string())
		.filter(|s| !s.is_empty())
		.collect())
}

fn prompt_keyword_group_and_reply() -> Result<(Vec<String>, String)> {
	let keywords = prompt_keyword_group_only()?;
	let reply = Input::<String>::with_theme(&theme())
		.with_prompt("设置该组关键词的回复内容")
		.interact_text()?;
	Ok((keywords, reply))
}

fn list_reply_groups(cur: &[KeywordGroupReply]) -> Vec<String> {
	cur.iter()
		.enumerate()
		.map(|(i, x)| {
			let tag = (b'A' + (i as u8)) as char;
			format!("{tag}. [{}] => {}", x.keywords.join(", "), truncate(&x.reply, 50))
		})
		.collect()
}

fn normalize_perm(s: &str) -> String {
	let u = s.trim().to_uppercase();
	match u.as_str() {
		"EVERY_MEMBER" | "ONLY_ADMINS" => u,
		"EVERY-MEMBER" => "EVERY_MEMBER".to_string(),
		"ONLY-ADMINS" => "ONLY_ADMINS".to_string(),
		_ => "EVERY_MEMBER".to_string(),
	}
}

fn run_daemon_front(gc: &GlobalConfig) -> Result<()> {
	let acc = gc
		.account
		.clone()
		.ok_or_else(|| anyhow!("未登录"))?;
	println!("[INF] 前台运行守护(按 Ctrl+C 退出) ...");
	run_daemon(&acc)
}

fn run_daemon(acc: &str) -> Result<()> {
	ensure_cmd("signal-cli")?;
	let mut gc = load_global()?;

	let (mut groups, self_id) = load_all_groups_runtime(acc, gc.signal_cli_config_dir.as_deref())?;
	if groups.is_empty() {
		return Err(anyhow!("No group configs found. 请先选择群组并保存配置。"));
	}

	println!("[INF] self_id = {self_id}");
	println!("[INF] watching {} group(s)", groups.len());

	let mut child = spawn_receive(acc, gc.signal_cli_config_dir.as_deref())?;
	let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
	let reader = BufReader::new(stdout);

	for line in reader.lines() {
		let line = match line {
			Ok(x) => x,
			Err(_) => break,
		};
		let line = line.trim();
		if line.is_empty() {
			continue;
		}
		let ev: ReceiveEnvelope = match serde_json::from_str(line) {
			Ok(v) => v,
			Err(_) => continue,
		};

		if let Some(dm) = &ev.envelope.data_message {
			if let Some(gi) = &dm.group_info {
				let gid = gi.group_id.clone();
				if !groups.contains_key(&gid) {
					continue;
				}
				let mut rt = groups.get(&gid).cloned().unwrap();
				handle_group_event(acc, &gc, &mut rt, &ev, dm, gi)?;
				groups.insert(gid, rt);
			}
		}
	}

	let _ = child.kill();
	Ok(())
}

fn spawn_receive(acc: &str, cfgdir: Option<&str>) -> Result<Child> {
	let mut cmd = Command::new("signal-cli");
	if let Some(d) = cfgdir {
		cmd.arg("--config").arg(d);
	}
	cmd.arg("-u").arg(acc).arg("-o").arg("json").arg("receive");
	cmd.arg("-t").arg("-1");
	cmd.arg("--ignore-attachments");
	cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
	let child = cmd.spawn().context("spawn signal-cli receive")?;
	Ok(child)
}

fn handle_group_event(
	acc: &str,
	gc: &GlobalConfig,
	rt: &mut GroupRuntime,
	ev: &ReceiveEnvelope,
	dm: &DataMessage,
	gi: &GroupInfo,
) -> Result<()> {
	let gid = rt.cfg.group_id.clone();

	if gi.kind == "UPDATE" {
		let prev_admin = rt.cfg.bot_has_admin;

		refresh_group_state(acc, gc.signal_cli_config_dir.as_deref(), rt)?;

		let now_admin = rt.cfg.bot_has_admin;

		if now_admin && rt.cfg.enabled {
			if rt.cfg.require_bot_admin_to_enforce {
				apply_takeover_permissions(acc, gc.signal_cli_config_dir.as_deref(), rt)?;
			}
		}

		let prev = rt.cfg.last_members_snapshot.clone();
		let cur = rt.members.clone();
		let added: Vec<String> = cur.difference(&prev).cloned().collect();
		if !added.is_empty() {
			rt.cfg.last_members_snapshot = cur.clone();
			save_group_cfg(&rt.cfg)?;

			if let Some(tpl) = &rt.cfg.welcome_template {
				for uid in added {
					let name = rt
						.member_names
						.get(&uid)
						.cloned()
						.unwrap_or_else(|| short_id(&uid));
					let msg = tpl.replace("##{@user}##", &name);
					let _ = send_group_message(acc, gc.signal_cli_config_dir.as_deref(), &gid, &msg);
				}
			}
		} else {
			rt.cfg.last_members_snapshot = cur.clone();
			save_group_cfg(&rt.cfg)?;
		}

		let _ = prev_admin;
		return Ok(());
	}

	let text = match &dm.message {
		Some(s) => s.trim().to_string(),
		None => return Ok(()),
	};
	if text.is_empty() {
		return Ok(());
	}

	let sender_id = ev
		.envelope
		.source_uuid
		.clone()
		.or_else(|| ev.envelope.source_number.clone())
		.or_else(|| ev.envelope.source.clone())
		.unwrap_or_else(|| "unknown".to_string());

	let sender_is_admin = rt.admins.contains(&sender_id);
	let bot_can_enforce = if rt.cfg.require_bot_admin_to_enforce {
		rt.cfg.bot_has_admin
	} else {
		true
	};

	if is_ban_command(&text) {
		if rt.cfg.only_admin_can_ban && !sender_is_admin {
			let _ = send_group_message(
				acc,
				gc.signal_cli_config_dir.as_deref(),
				&gid,
				"无权限：仅管理员可执行 /ban。",
			);
			return Ok(());
		}
		if !bot_can_enforce {
			let _ = send_group_message(
				acc,
				gc.signal_cli_config_dir.as_deref(),
				&gid,
				"Bot 无管理员权限，已暂停踢人/警告。",
			);
			return Ok(());
		}

		let mut target: Option<String> = dm
			.quote
			.as_ref()
			.and_then(|q| q.author.clone())
			.filter(|s| !s.trim().is_empty());

		if target.is_none() {
			target = extract_target_from_text(&text);
		}

		let Some(t) = target else {
			let _ = send_group_message(
				acc,
				gc.signal_cli_config_dir.as_deref(),
				&gid,
				"用法：回复目标消息发送 /ban@magicbot 或 /ban@magicbot <uuid/号码>。",
			);
			return Ok(());
		};

		match remove_member(acc, gc.signal_cli_config_dir.as_deref(), &gid, &t) {
			Ok(_) => {
				let _ = send_group_message(acc, gc.signal_cli_config_dir.as_deref(), &gid, "已移出群组。");
			}
			Err(e) => {
				let _ = send_group_message(
					acc,
					gc.signal_cli_config_dir.as_deref(),
					&gid,
					&format!("踢人失败：{e}"),
				);
			}
		}
		return Ok(());
	}

	if !rt.cfg.enabled {
		return Ok(());
	}

	if !bot_can_enforce && (hit_any_rule(&rt.cfg.warn_rules, &text) || hit_any_rule_ban(&rt.cfg.ban_rules, &text))
	{
		let _ = send_group_message(acc, gc.signal_cli_config_dir.as_deref(), &gid, "Bot 无管理员权限，已暂停踢人/警告。");
		return Ok(());
	}

	if bot_can_enforce && hit_any_rule_ban(&rt.cfg.ban_rules, &text) {
		let _ = remove_member(acc, gc.signal_cli_config_dir.as_deref(), &gid, &sender_id);
		clear_warn_mark(&gid, &sender_id)?;
		return Ok(());
	}

	if bot_can_enforce && hit_any_rule(&rt.cfg.warn_rules, &text) {
		let kicked = warn_and_maybe_kick(acc, gc.signal_cli_config_dir.as_deref(), rt, &sender_id)?;
		if kicked {
			let _ = send_group_message(acc, gc.signal_cli_config_dir.as_deref(), &gid, "已因多次警告移出群组。");
		} else {
			let _ = send_group_message(acc, gc.signal_cli_config_dir.as_deref(), &gid, &rt.cfg.warn_message);
		}
		return Ok(());
	}

	for r in &rt.cfg.auto_replies {
		if keywords_match(&r.keywords, &text) {
			let _ = send_group_message(acc, gc.signal_cli_config_dir.as_deref(), &gid, &r.reply);
			break;
		}
	}

	Ok(())
}

fn is_ban_command(s: &str) -> bool {
	let t = s.trim();
	t.starts_with("/ban") || t.starts_with("/ban@") || t.contains("/ban@magicbot")
}

fn extract_target_from_text(s: &str) -> Option<String> {
	let re_uuid = Regex::new(r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})").ok()?;
	if let Some(c) = re_uuid.captures(s) {
		return Some(c[1].to_string());
	}
	let re_phone = Regex::new(r"(\+\d{6,20})").ok()?;
	if let Some(c) = re_phone.captures(s) {
		return Some(c[1].to_string());
	}
	None
}

fn keywords_match(keywords: &[String], text: &str) -> bool {
	let lower = text.to_lowercase();
	keywords.iter().any(|k| {
		let kk = k.trim().to_lowercase();
		!kk.is_empty() && lower.contains(&kk)
	})
}

fn hit_any_rule(rules: &[KeywordGroupWarn], text: &str) -> bool {
	for r in rules {
		if keywords_match(&r.keywords, text) {
			return true;
		}
	}
	false
}

fn hit_any_rule_ban(rules: &[KeywordGroupBan], text: &str) -> bool {
	for r in rules {
		if keywords_match(&r.keywords, text) {
			return true;
		}
	}
	false
}

fn warn_mark_path(gid: &str, user: &str) -> PathBuf {
	group_mark_dir(gid).join(format!("{user}.json"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WarnMark {
	first_ts: i64,
	count: u32,
}

fn warn_and_maybe_kick(acc: &str, cfgdir: Option<&str>, rt: &GroupRuntime, user: &str) -> Result<bool> {
	let gid = &rt.cfg.group_id;
	fs::create_dir_all(group_mark_dir(gid))?;
	let p = warn_mark_path(gid, user);
	let now = Utc::now().timestamp();

	let mut mark = if p.exists() {
		let mut s = String::new();
		File::open(&p)?.read_to_string(&mut s)?;
		serde_json::from_str::<WarnMark>(&s).unwrap_or(WarnMark { first_ts: now, count: 0 })
	} else {
		WarnMark { first_ts: now, count: 0 }
	};

	let window = (rt.cfg.warn_window_minutes as i64) * 60;
	if now - mark.first_ts > window {
		mark.first_ts = now;
		mark.count = 0;
	}
	mark.count += 1;

	fs::write(&p, serde_json::to_vec_pretty(&mark)?)?;

	if mark.count > rt.cfg.warn_max_count {
		let _ = remove_member(acc, cfgdir, gid, user);
		clear_warn_mark(gid, user)?;
		return Ok(true);
	}

	Ok(false)
}

fn clear_warn_mark(gid: &str, user: &str) -> Result<()> {
	let p = warn_mark_path(gid, user);
	if p.exists() {
		let _ = fs::remove_file(p);
	}
	Ok(())
}

fn apply_takeover_permissions(acc: &str, cfgdir: Option<&str>, rt: &GroupRuntime) -> Result<()> {
	if !rt.cfg.bot_has_admin {
		return Ok(());
	}

	let gid = &rt.cfg.group_id;
	let mut cmd = Command::new("signal-cli");
	if let Some(d) = cfgdir {
		cmd.arg("--config").arg(d);
	}
	cmd.arg("-u").arg(acc).arg("updateGroup").arg("-g").arg(gid);

	cmd.arg("--set-permission-add-member").arg(&rt.cfg.desired_permission_add_member);
	cmd.arg("--set-permission-send-messages").arg(&rt.cfg.desired_permission_send_message);
	cmd.arg("--set-permission-edit-details").arg(&rt.cfg.desired_permission_edit_details);

	let _ = run_ok(&mut cmd);
	Ok(())
}

fn remove_member(acc: &str, cfgdir: Option<&str>, gid: &str, who: &str) -> Result<()> {
	let mut cmd = Command::new("signal-cli");
	if let Some(d) = cfgdir {
		cmd.arg("--config").arg(d);
	}
	cmd.arg("-u").arg(acc).arg("updateGroup").arg("-g").arg(gid);
	cmd.arg("--remove-member").arg(who);
	run_ok(&mut cmd)?;
	Ok(())
}

fn send_group_message(acc: &str, cfgdir: Option<&str>, gid: &str, msg: &str) -> Result<()> {
	let mut cmd = Command::new("signal-cli");
	if let Some(d) = cfgdir {
		cmd.arg("--config").arg(d);
	}
	cmd.arg("-u").arg(acc).arg("send").arg("-g").arg(gid).arg("-m").arg(msg);
	run_ok(&mut cmd)?;
	Ok(())
}

fn refresh_group_state(acc: &str, cfgdir: Option<&str>, rt: &mut GroupRuntime) -> Result<()> {
	let groups = list_groups_full(acc, cfgdir)?;
	let g = groups
		.iter()
		.find(|x| x.id == rt.cfg.group_id)
		.ok_or_else(|| anyhow!("group not found"))?;

	let self_id = rt.self_id.clone();

	let admins = g.admins.iter().map(|i| i.id.clone()).collect::<BTreeSet<_>>();
	let members = g.members.iter().map(|i| i.id.clone()).collect::<BTreeSet<_>>();

	let bot_admin = admins.contains(&self_id);

	rt.admins = admins;
	rt.members = members;
	rt.cfg.bot_has_admin = bot_admin;

	rt.member_names = build_identity_name_map(acc, cfgdir)?;
	for m in &g.members {
		if let Some(n) = &m.name {
			rt.member_names.insert(m.id.clone(), n.clone());
		}
	}

	if rt.cfg.last_members_snapshot.is_empty() {
		rt.cfg.last_members_snapshot = rt.members.clone();
	}
	save_group_cfg(&rt.cfg)?;
	Ok(())
}

fn short_id(s: &str) -> String {
	if s.len() <= 12 {
		return s.to_string();
	}
	format!("{}…{}", &s[0..6], &s[s.len() - 4..])
}

fn truncate(s: &str, n: usize) -> String {
	let mut out = String::new();
	for (i, ch) in s.chars().enumerate() {
		if i >= n {
			out.push('…');
			break;
		}
		out.push(ch);
	}
	out
}

fn systemd_menu(gc: &mut GlobalConfig) -> Result<()> {
	require_root()?;
	let items = vec![
		"1. 安装/覆盖 systemd unit",
		"2. 启用开机自启",
		"3. 禁用开机自启",
		"4. 启动服务",
		"5. 停止服务",
		"6. 查看状态",
		"7. 卸载 unit",
		"0. 返回",
	];
	let sel = Select::with_theme(&theme()).items(&items).default(0).interact()?;
	match sel {
		0 => install_systemd_unit()?,
		1 => {
			run_ok(Command::new("systemctl").arg("enable").arg("--now").arg("magicbot"))?;
			gc.daemon_enabled = true;
			save_global(gc)?;
		}
		2 => {
			run_ok(Command::new("systemctl").arg("disable").arg("--now").arg("magicbot"))?;
			gc.daemon_enabled = false;
			save_global(gc)?;
		}
		3 => {
			run_ok(Command::new("systemctl").arg("start").arg("magicbot"))?;
		}
		4 => {
			run_ok(Command::new("systemctl").arg("stop").arg("magicbot"))?;
		}
		5 => {
			let _ = Command::new("systemctl").arg("status").arg("magicbot").arg("-l").status();
		}
		6 => uninstall_systemd_unit()?,
		_ => {}
	}
	Ok(())
}

fn install_systemd_unit() -> Result<()> {
	let exe = env::current_exe().context("current_exe")?;
	let content = format!(
		"[Unit]
Description=MagicBot (Signal) daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={} --daemon
Restart=always
RestartSec=2
User=root
WorkingDirectory=/
Environment=RUST_BACKTRACE=1

[Install]
WantedBy=multi-user.target
",
		exe.display()
	);

	fs::write(SYSTEMD_UNIT, content)?;
	run_ok(Command::new("systemctl").arg("daemon-reload"))?;
	println!("[OK] Installed unit: {SYSTEMD_UNIT}");
	Ok(())
}

fn uninstall_systemd_unit() -> Result<()> {
	if Path::new(SYSTEMD_UNIT).exists() {
		let _ = Command::new("systemctl").arg("disable").arg("--now").arg("magicbot").status();
		let _ = fs::remove_file(SYSTEMD_UNIT);
		let _ = Command::new("systemctl").arg("daemon-reload").status();
		println!("[OK] Removed unit.");
	}
	Ok(())
}

fn captcha_menu(gc: &GlobalConfig) -> Result<()> {
	let acc = gc.account.clone().ok_or_else(|| anyhow!("未登录"))?;
	println!("\n[INF] 打开网站生成 token：");
	println!("  https://signalcaptchas.org/challenge/generate.html");
	println!("[INF] 复制得到的 signalcaptcha://... 粘贴到这里。\n");

	let cap = Input::<String>::with_theme(&theme())
		.with_prompt("Captcha token (signalcaptcha://...)")
		.interact_text()?;

	let chal = Input::<String>::with_theme(&theme())
		.with_prompt("如有 challenge token 可填(留空跳过)")
		.allow_empty(true)
		.interact_text()?;

	let mut cmd = Command::new("signal-cli");
	if let Some(dir) = &gc.signal_cli_config_dir {
		cmd.arg("--config").arg(dir);
	}
	cmd.arg("-u").arg(&acc).arg("submitRateLimitChallenge");
	if !chal.trim().is_empty() {
		cmd.arg("--challenge").arg(chal.trim());
	}
	cmd.arg("--captcha").arg(cap.trim());
	run_ok(&mut cmd)?;
	println!("[OK] 已提交。");
	Ok(())
}

fn logout_and_cleanup(gc: &mut GlobalConfig) -> Result<()> {
	require_root()?;
	let acc = gc.account.clone().unwrap_or_default();
	if acc.is_empty() {
		println!("[WRN] 未登录。");
		return Ok(());
	}

	println!("[WRN] 退出登录会清理 magicbot 本机配置与计数标记。");
	let also_delete_signal = Confirm::with_theme(&theme())
		.with_prompt("是否同时删除 signal-cli 本机账号数据(危险，需重新注册/绑定)?")
		.default(false)
		.interact()?;

	if Confirm::with_theme(&theme())
		.with_prompt("确认执行？")
		.default(false)
		.interact()?
	{
		let _ = fs::remove_dir_all(groups_dir());
		let _ = fs::remove_dir_all(PathBuf::from(STATE_DIR).join("marks"));

		gc.selected_group = None;
		gc.account = None;
		save_global(gc)?;

		if also_delete_signal {
			let mut cmd = Command::new("signal-cli");
			if let Some(dir) = &gc.signal_cli_config_dir {
				cmd.arg("--config").arg(dir);
			}
			cmd.arg("-u").arg(&acc).arg("deleteLocalAccountData").arg("--ignore-registered");
			let _ = run_ok(&mut cmd);
		}

		println!("[OK] 已退出并清理。");
	}

	Ok(())
}

#[derive(Clone, Debug)]
struct GroupSummary {
	id: String,
	name: String,
}

#[derive(Clone, Debug)]
struct GroupFull {
	id: String,
	name: String,
	admins: Vec<Identity>,
	members: Vec<Identity>,
}

fn list_groups(acc: &str, cfgdir: Option<&str>) -> Result<Vec<GroupSummary>> {
	let v = run_signal_json(Command::new("signal-cli"), cfgdir, Some(acc), &["-o", "json", "listGroups"])?;
	let arr = v.as_array().ok_or_else(|| anyhow!("listGroups not array"))?;
	let mut out = vec![];
	for g in arr {
		let id = g.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
		let name = g.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
		if !id.is_empty() {
			out.push(GroupSummary { id, name });
		}
	}
	Ok(out)
}

fn list_groups_full(acc: &str, cfgdir: Option<&str>) -> Result<Vec<GroupFull>> {
	let v = run_signal_json(Command::new("signal-cli"), cfgdir, Some(acc), &["-o", "json", "listGroups"])?;
	let arr = v.as_array().ok_or_else(|| anyhow!("listGroups not array"))?;
	let mut out = vec![];
	for g in arr {
		let id = g.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
		let name = g.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();

		let admins = parse_identities(g.get("admins"));
		let members = parse_identities(g.get("members"));

		if !id.is_empty() {
			out.push(GroupFull { id, name, admins, members });
		}
	}
	Ok(out)
}

fn parse_identities(v: Option<&Value>) -> Vec<Identity> {
	let mut out = vec![];
	let Some(v) = v else { return out };
	let Some(arr) = v.as_array() else { return out };

	for it in arr {
		let uuid = it.get("uuid").and_then(|x| x.as_str()).map(|s| s.to_string());
		let number = it.get("number").and_then(|x| x.as_str()).map(|s| s.to_string());

		let id = uuid.or(number.clone()).unwrap_or_else(|| "".to_string());
		if id.is_empty() {
			continue;
		}

		out.push(Identity { id, number, name: None });
	}
	out
}

fn build_identity_name_map(acc: &str, cfgdir: Option<&str>) -> Result<HashMap<String, String>> {
	let mut map = HashMap::new();
	let v = run_signal_json(
		Command::new("signal-cli"),
		cfgdir,
		Some(acc),
		&["-o", "json", "listContacts", "--all-recipients", "--detailed"],
	)?;
	let empty = vec![];
	let arr = v.as_array().unwrap_or(&empty);
	for c in arr {
		let uuid = c.get("uuid").and_then(|x| x.as_str()).map(|s| s.to_string());
		let number = c.get("number").and_then(|x| x.as_str()).map(|s| s.to_string());
		let name = c.get("name").and_then(|x| x.as_str()).map(|s| s.to_string());

		if let Some(n) = name {
			if let Some(u) = uuid.clone() {
				map.insert(u, n.clone());
			}
			if let Some(p) = number.clone() {
				map.insert(p, n);
			}
		}
	}
	Ok(map)
}

fn list_local_accounts(gc: &GlobalConfig) -> Result<Vec<String>> {

	let mut cmd = Command::new("signal-cli");
	if let Some(dir) = &gc.signal_cli_config_dir {
		cmd.arg("--config").arg(dir);
	}
	cmd.arg("-o").arg("json").arg("listGroups");

	let out = cmd.output()?;
	let s = String::from_utf8_lossy(&out.stdout).to_string();
	if s.trim().is_empty() {
		return Ok(vec![]);
	}

	let v: Value = serde_json::from_str(&s)?;
	let empty = vec![];
	let arr = v.as_array().unwrap_or(&empty);
	let mut cand = BTreeSet::new();
	for g in arr {
		if let Some(m) = g.get("members").and_then(|x| x.as_array()) {
			for it in m {
				if let Some(n) = it.get("number").and_then(|x| x.as_str()) {
					if n.starts_with('+') {
						cand.insert(n.to_string());
					}
				}
			}
		}
	}

	Ok(cand.into_iter().collect())
}

fn load_all_groups_runtime(acc: &str, cfgdir: Option<&str>) -> Result<(HashMap<String, GroupRuntime>, String)> {
	let full = list_groups_full(acc, cfgdir)?;
	let mut runtime = HashMap::new();

	let mut self_id = acc.to_string();
	for g in &full {
		for m in &g.members {
			if let Some(num) = &m.number {
				if num == acc {
					self_id = m.id.clone();
					break;
				}
			}
		}
	}

	for g in &full {
		let p = group_cfg_path(&g.id);
		if !p.exists() {
			continue;
		}
		let mut cfg = load_group_cfg(&g.id)?;
		if cfg.group_name.is_empty() {
			cfg.group_name = g.name.clone();
		}

		let admins = g.admins.iter().map(|i| i.id.clone()).collect::<BTreeSet<_>>();
		let members = g.members.iter().map(|i| i.id.clone()).collect::<BTreeSet<_>>();

		cfg.bot_has_admin = admins.contains(&self_id);

		let mut member_names = build_identity_name_map(acc, cfgdir)?;
		for m in &g.members {
			if let Some(n) = &m.name {
				member_names.insert(m.id.clone(), n.clone());
			}
		}

		if cfg.last_members_snapshot.is_empty() {
			cfg.last_members_snapshot = members.clone();
		}

		save_group_cfg(&cfg)?;

		runtime.insert(
			g.id.clone(),
			GroupRuntime {
				cfg,
				admins,
				members,
				member_names,
				self_id: self_id.clone(),
			},
		);
	}

	Ok((runtime, self_id))
}

fn run_signal_json(mut base: Command, cfgdir: Option<&str>, acc: Option<&str>, args: &[&str]) -> Result<Value> {
	if let Some(d) = cfgdir {
		base.arg("--config").arg(d);
	}
	if let Some(a) = acc {
		base.arg("-u").arg(a);
	}
	for x in args {
		base.arg(x);
	}
	let out = base.output()?;
	if !out.status.success() {
		return Err(anyhow!(
			"signal-cli failed: {}",
			String::from_utf8_lossy(&out.stderr)
		));
	}
	let s = String::from_utf8_lossy(&out.stdout).to_string();
	let v: Value = serde_json::from_str(&s).context("parse json")?;
	Ok(v)
}

fn ensure_cmd(cmd: &str) -> Result<()> {
	let ok = Command::new("bash")
		.arg("-lc")
		.arg(format!("command -v {cmd} >/dev/null 2>&1"))
		.status()?
		.success();
	if !ok {
		return Err(anyhow!("Missing command: {cmd}"));
	}
	Ok(())
}

fn require_root() -> Result<()> {
	if unsafe { libc::geteuid() } != 0 {
		return Err(anyhow!("Must run as root."));
	}
	Ok(())
}

fn read_os_id() -> Result<String> {
	let s = fs::read_to_string("/etc/os-release").context("read /etc/os-release")?;
	for line in s.lines() {
		if let Some(v) = line.strip_prefix("ID=") {
			return Ok(v.trim().trim_matches('"').to_string());
		}
	}
	Err(anyhow!("Cannot detect OS ID"))
}

fn run_ok(cmd: &mut Command) -> Result<()> {
	let out = cmd.output().context("run command")?;
	if !out.status.success() {
		return Err(anyhow!(
			"Command failed. stderr={}",
			String::from_utf8_lossy(&out.stderr)
		));
	}
	Ok(())
}
