//! 设置页「账号管理」的多账号后端。
//!
//! # 为什么需要
//!
//! Phira 只有**邮箱 + 密码**登录，没有长期 token。原来的客户端只有「一个当前账号」
//! （`Data::me` + `Data::tokens`），想换号只能退出再重新输一遍邮箱密码。
//! 而移动端此前只有 WebView 开始页、里面没有任何账号入口，**根本没法登录**。
//!
//! 这里提供「保存多个账号 + 一键切换」：
//! - 凭据存在 `Data::accounts`（本机 `data.json`）；
//! - 切换 = 用保存的凭据重新登录（Phira 没有 token，这是唯一可靠的做法）；
//! - 展示用的 `name` / `avatar` / `rks` 一起缓存，离线也能显示列表。
//!
//! 界面在 `crate::page::settings` 的「账号管理」一节。

use crate::{
    client::{Client, LoginParams},
    get_data, get_data_mut, save_data, sync_data,
    data::SavedAccount,
};
use anyhow::Result;
use tracing::{info, warn};

/// 把当前登录的账号（`Data::me` + 凭据）写进 `Data::accounts`。
///
/// ⚠️ 目前**没有调用点**：登录已经改用 Phira 自带的登录界面（`crate::login::Login`，
/// 见 `page/settings.rs` 的 `AccountList::ensure_login`），而那个组件只写 `data.me`、
/// **不暴露密码** —— 拿不到密码就存不进 `accounts`，所以「登录后自动记住以便一键切换」
/// 这条链路暂时断了。已保存过的账号（`accounts` 里已有的）切换仍然正常。
///
/// 保留本函数是为了将来要么给自带登录界面加凭据回调、要么恢复一条「保存密码」的入口。
/// 已存在同邮箱的条目就更新（改名 / 换头像 / RKS 变化都能同步）。
#[allow(dead_code)]
pub fn remember_account(email: &str, password: &str, name: String, avatar: String, rks: f32) {
    if email.is_empty() {
        return;
    }
    let data = get_data_mut();
    let entry = SavedAccount {
        email: email.to_owned(),
        password: password.to_owned(),
        name,
        avatar,
        rks,
    };
    match data.accounts.iter_mut().find(|it| it.email == email) {
        Some(slot) => *slot = entry,
        None => data.accounts.push(entry),
    }
}

/// `User::avatar` 是 `Option<File>`（`File` 就是个包着 `url` 的透明结构），
/// 列表里只要 URL 字符串，取不到就是空串（界面会用名字首字母兜底）。
fn avatar_url(user: &crate::client::User) -> String {
    user.avatar.as_ref().map(|it| it.url.clone()).unwrap_or_default()
}

/// 用保存的凭据登录某个账号，成功后设为当前账号。
///
/// 返回登录后的展示名。
pub async fn switch_account(email: &str) -> Result<String> {
    // 先把凭据取出来（后面要 await，不能一直持有借用）
    let (password, _cached_name) = {
        let data = get_data();
        match data.accounts.iter().find(|it| it.email == email) {
            Some(it) => (it.password.clone(), it.name.clone()),
            None => anyhow::bail!("找不到已保存的账号：{email}"),
        }
    };
    if password.is_empty() {
        anyhow::bail!("该账号没有保存密码，请删除后重新添加");
    }

    Client::login(LoginParams::Password {
        email: email,
        password: password.as_str(),
    })
    .await?;

    let me = Client::get_me().await?;
    let (name, avatar, rks) = (me.name.clone(), avatar_url(&me), me.rks);
    {
        let data = get_data_mut();
        data.me = Some(me);
        // 同步刷新缓存资料，列表下次离线也能显示最新的名字/头像
        if let Some(slot) = data.accounts.iter_mut().find(|it| it.email == email) {
            slot.name = name.clone();
            slot.avatar = avatar.clone();
            slot.rks = rks;
        }
    }
    save_data()?;
    sync_data();
    info!(email, %name, "账号已切换");
    Ok(name)
}

/// 登录一个**新**账号并保存下来。
///
/// ⚠️ 同 [`remember_account`]：目前没有调用点（登录已改用 Phira 自带登录界面）。
#[allow(dead_code)]
pub async fn add_account(email: &str, password: &str) -> Result<String> {
    if email.is_empty() || password.is_empty() {
        anyhow::bail!("邮箱和密码都不能为空");
    }
    Client::login(LoginParams::Password { email, password }).await?;
    let me = Client::get_me().await?;
    let (name, avatar, rks) = (me.name.clone(), avatar_url(&me), me.rks);
    {
        let data = get_data_mut();
        data.me = Some(me);
    }
    remember_account(email, password, name.clone(), avatar, rks);
    save_data()?;
    sync_data();
    info!(email, %name, "已添加并登录账号");
    Ok(name)
}

/// 从列表里删除一个账号（**不会**退出当前登录）。界面负责询问确认。
pub fn forget_account(email: &str) -> Result<()> {
    let data = get_data_mut();
    let before = data.accounts.len();
    data.accounts.retain(|it| it.email != email);
    if data.accounts.len() == before {
        warn!(email, "要删除的账号不在列表里");
    }
    save_data()
}

/// 退出当前账号（与 WebView 的 `logout` IPC 行为一致）。
pub fn logout() -> Result<()> {
    {
        let data = get_data_mut();
        data.me = None;
        data.tokens = None;
    }
    save_data()?;
    sync_data();
    Ok(())
}

/// 当前登录账号的邮箱（用于在列表里标出「当前」，并判断要不要显示切换按钮）。
///
/// `Data::me`（`client::User`）里只有 `name` / `id`，**没有邮箱**；而 Phira 用户名唯一，
/// 所以用「名字匹配」反查。登录 / 添加 / 切换时我们都会把 `name` 一起缓存进 `accounts`。
pub fn current_email() -> Option<String> {
    let me = get_data().me.clone()?;
    get_data()
        .accounts
        .iter()
        .find(|it| it.name == me.name)
        .map(|it| it.email.clone())
}
