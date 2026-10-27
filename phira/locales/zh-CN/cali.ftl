
label = 延迟校准

# ---- 准度条编辑器（`page/timing_editor.rs`）----
# 并进这个 ftl 而不是新开 `timing_editor.ftl`：新增 ftl 文件必须同时放进全部 15 个
# 语言目录（`create_bundles!` 对每个 locale 都 `include_str!`），为一个页面开新文件
# 不划算。两者同属「校准」类功能，放一起也说得通。
timing-editor-title = 准度条编辑
timing-editor-save = 保存
timing-editor-hide = 隐藏面板
timing-editor-show = 显示面板
timing-editor-style = 样式
timing-editor-style-curved = 弧形
timing-editor-style-line = 直线
timing-editor-record-seconds = 记录时间
timing-editor-x = 横坐标
timing-editor-y = 纵坐标
timing-editor-size = 大小
