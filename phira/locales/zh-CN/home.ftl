
play = 游玩
event = 活动
respack = 资源包

not-opened = 功能尚未开启
not-logged-in = 未登录

failed-to-update = 加载用户信息失败
note-try-login-again = 提示: 请尝试重新登录

update = v{ $version } 更新
update-desc =
  时间：{ $date }
  描述：{ $desc }
update-ignore = 忽略该版本
update-go = 更新

change-char = 更换角色

random-pick = 随机
random-no-chart = 还没有导入任何谱面

menu-start = 开始游戏
menu-messages = 消息
menu-settings = 设置
menu-about = 关于
menu-quit = 退出

about-desc = 社区驱动的音游平台

# ⚠️ `char-imported` 原来只写在 `settings.ftl` 里，而 `home.rs` 的 `tl!` 绑定的是
# 本文件 —— 跨文件的 key 不会互相回退，所以首页一直显示的是字面量 "char-imported"。
# 这里补一份本文件自己的。
char-imported = 立绘已导入，重启 App 后生效
char-import-failed = 立绘导入失败
hint-character-detail = 点击左侧立绘查看角色详情
last-played-none = 上次游玩的谱面
