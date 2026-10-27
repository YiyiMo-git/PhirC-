// iOS WebView 界面宿主（WKWebView）。
//
// 只被 phira/build.rs 在 iOS 目标下编译（见 `build_ios_webview`），其它平台不参与构建
// —— 和 `ios/motion.m` / `ios/ime.m` 同一套路。对外只暴露一组 C 函数，
// Rust 侧在 `phira/src/webui.rs` 的 iOS 段里声明同名 extern。
//
// # 为什么是 ObjC 而不是 objc2
//
// 工程 iOS 侧已有先例（motion.m / ime.m）：用 C 函数做边界最简单可靠；
// `WKScriptMessageHandler` / `WKURLSchemeHandler` 是协议实现，objc2 写协议一致性冗长且难调。
//
// # 资源怎么喂（与 Windows 完全对齐的关键）
//
// 用 **`WKURLSchemeHandler` 实现 `phirc://`**（不要走 `loadHTMLString` 内联——那是旧方案的
// 妥协，页面一多就崩；也不要用 `file://`——`texImage2D` 会判跨源抛 SecurityError）。
// 自定义 scheme 下页面里的相对路径原样解析成 `phirc://web/...`，与 Windows 的
// `phirc://web/<path>` 自定义协议一字不差：`WebCmd::Navigate` 的 URL **不用翻译**，
// `serve_asset` / `serve_respack` / `local-cover` 全部由 Rust 回调 [`phira_webui_asset`] 提供。
//
// # 与旧 webui.m 的两个重要差别
//
// 1. **不禁 `userInteractionEnabled`**。旧方案因 JS 注入不生效（`loadHTMLString` 的坑）
//    干脆禁掉触摸、用 Rust 矩形命中按钮。现在是正常导航 + `WKUserScript`（DocumentStart），
//    页面需要完整交互（触点 / 输入框 / 弹窗），绝不能禁。
// 2. 不包含软键盘桥 —— 那部分已拆到 `ios/ime.m`（Rust 侧 `phira::ime`），两份文件
//    不能重复定义同一批符号。
//
// # 线程
//
// WKWebView 必须在主线程操作。Rust 从游戏线程调用，这里统一 `dispatch_async`
// 到主队列；函数本身立即返回（即发即忘）。scheme handler 由 WebKit 在其工作线程回调，
// Rust 读文件是本地 IO，同步返回即可（`didReceive*` 系列必须在 start 调用线程上回）。
//
// # 内存管理
//
// 不启用 ARC（与 motion.m / ime.m 一致），静态持有的对象手动 release。

#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>
#import <WebKit/WebKit.h>

/// Rust 侧提供的回调：页面发来一条 IPC 消息（JSON 字符串）时调用。
typedef void (*phira_webui_ipc_fn)(const char *json);

@class PhiraWebUIMessageHandler;
@class PhiraWebUISchemeHandler;

static WKWebView *gWebView = nil;
static UIViewController *gController = nil;
static PhiraWebUIMessageHandler *gHandler = nil;
static PhiraWebUISchemeHandler *gScheme = nil;
static phira_webui_ipc_fn gIpcFn = NULL;

/// 界面脚本（`init_scripts()`）的副本，供 `didFinishNavigation` 兜底再注入。
/// MRC 下由 `create` 接管所有权（copy），`destroy` 释放。
static NSString *gInitJs = nil;

// ---- Rust 侧导出（webui.rs iOS 段，#[no_mangle] extern "C"）----
/// 页面 `window.webkit.messageHandlers.phira.postMessage(json)` → Rust `handle_ipc`。
extern void phira_webui_ipc(const char *json);
/// 取一个 Web 资源。`path_and_query` 形如 `/index.html` 或 `/local-cover?...`。
/// 成功时返回 1 并填 `mime_id` / `data` / `len`；`data` 由 Rust `malloc`，ObjC 用完 `free`。
extern int phira_webui_asset(const char *path_and_query, int *mime_id, const void **data, unsigned long long *len);

/// MIME 枚举（Rust 侧 `mime_id_of` 填，这里查表成 Content-Type）。
static NSString *mime_string(int id) {
    switch (id) {
        case 0: return @"text/html";
        case 1: return @"application/javascript";
        case 2: return @"text/css";
        case 3: return @"image/png";
        case 4: return @"image/jpeg";
        case 5: return @"image/webp";
        case 6: return @"font/ttf";
        case 7: return @"font/otf";
        case 8: return @"application/json";
        default: return @"application/octet-stream";
    }
}

#pragma mark - 消息 / 导航代理

/// 接收页面 `window.webkit.messageHandlers.phira.postMessage(...)` 的消息。
///
/// `WKScriptMessage.body` 传 JavaScript 字符串进来就是 `NSString`。
@interface PhiraWebUIMessageHandler : NSObject <WKScriptMessageHandler, WKNavigationDelegate>
@end

@implementation PhiraWebUIMessageHandler

- (void)userContentController:(WKUserContentController *)userContentController
      didReceiveScriptMessage:(WKScriptMessage *)message {
    (void)userContentController;
    if (gIpcFn == NULL) {
        return;
    }
    id body = message.body;
    if (![body isKindOfClass:[NSString class]]) {
        return;
    }
    const char *json = [(NSString *)body UTF8String];
    if (json != NULL) {
        gIpcFn(json);
    }
}

/// 页面加载完成：兜底再注入一次界面脚本（与 Windows 的
/// `with_initialization_script` 双保险同一思路；脚本是幂等赋值，重复注入无害）。
- (void)webView:(WKWebView *)webView didFinishNavigation:(WKNavigation *)navigation {
    (void)webView;
    (void)navigation;
    if (gInitJs != nil && gWebView != nil) {
        [gWebView evaluateJavaScript:gInitJs completionHandler:nil];
    }
    NSLog(@"[webui-ios] 页面加载完成");
}

@end

#pragma mark - phirc:// 自定义协议（与 Windows 的 phirc:// 对齐）

/// `phirc://web/<path>` → Rust `phira_webui_asset` → 返回字节。
///
/// `local-cover` 带 query（`?p=<base64url>`），而 `URL.path` 不含 query ——
/// 所以这里把 `path?query` 拼在一起传给 Rust，由 Rust 侧分流。
@interface PhiraWebUISchemeHandler : NSObject <WKURLSchemeHandler>
@end

@implementation PhiraWebUISchemeHandler

- (void)webView:(WKWebView *)webView startURLSchemeTask:(id<WKURLSchemeTask>)task {
    (void)webView;
    NSURL *url = task.request.URL;
    NSString *pathAndQuery = url.path ?: @"/";
    if (url.query.length > 0) {
        pathAndQuery = [pathAndQuery stringByAppendingFormat:@"?%@", url.query];
    }

    int mimeId = 9;
    const void *data = NULL;
    unsigned long long len = 0;
    int ok = phira_webui_asset([pathAndQuery UTF8String], &mimeId, &data, &len);
    if (!ok || data == NULL) {
        // 404：与 Windows 的 protocol_response 行为一致（找不到就给错误码）
        NSError *err = [NSError errorWithDomain:NSURLErrorDomain
                                           code:NSURLErrorFileDoesNotExist
                                       userInfo:@{NSURLErrorKey: url}];
        [task didFailWithError:err];
        return;
    }

    NSData *bytes = [NSData dataWithBytes:data length:(NSUInteger)len];
    free((void *)data);
    NSDictionary *headers = @{
        @"Content-Type": mime_string(mimeId),
        @"Cache-Control": @"no-cache",
    };
    NSHTTPURLResponse *resp = [[NSHTTPURLResponse alloc] initWithURL:url
                                                          statusCode:200
                                                         HTTPVersion:@"HTTP/1.1"
                                                        headerFields:headers];
    [task didReceiveResponse:resp];
    [task didReceiveData:bytes];
    [task didFinishLoading];
    [resp release];
}

- (void)webView:(WKWebView *)webView stopURLSchemeTask:(id<WKURLSchemeTask>)task {
    (void)webView;
    (void)task;
    // 同步喂数据，没有后台任务要取消。
}

@end

#pragma mark - 辅助

/// 取出当前 app 的根视图控制器（WKWebView 要挂到它的 view 上）。
/// miniquad 把游戏视图（QuadView）设成了 window.rootViewController.view，
/// WebView 叠在上面（与安卓把 WebView 叠进 FrameLayout 同构）。
static UIViewController *phira_root_view_controller(void) {
    for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
        if (![scene isKindOfClass:[UIWindowScene class]]) {
            continue;
        }
        for (UIWindow *window in ((UIWindowScene *)scene).windows) {
            if (window.isKeyWindow && window.rootViewController != nil) {
                return window.rootViewController;
            }
        }
    }
    // 兜底：老式 AppDelegate window
    for (UIWindow *window in UIApplication.sharedApplication.windows) {
        if (window.isKeyWindow && window.rootViewController != nil) {
            return window.rootViewController;
        }
    }
    return nil;
}

#pragma mark - C 边界（Rust 调）

/// 建 WKWebView（隐藏）并注册桥 / 协议。幂等。
///
/// `init_js` 以 **WKUserScript（DocumentStart）** 注入 —— 早于页面自身脚本，
/// 与 Windows `with_initialization_script` 语义一致；`didFinishNavigation` 里还会兜底再注入。
void phira_webui_create(const char *init_js, phira_webui_ipc_fn ipc_fn) {
    gIpcFn = ipc_fn;
    NSString *initStr = (init_js != NULL) ? [NSString stringWithUTF8String:init_js] : nil;

    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView != nil) {
            return;
        }
        UIViewController *root = phira_root_view_controller();
        if (root == nil || root.view == nil) {
            NSLog(@"[webui-ios] 找不到根视图控制器，WebView 无法创建");
            return;
        }
        gController = [root retain];

        WKWebViewConfiguration *cfg = [[WKWebViewConfiguration alloc] init];

        // 页面按屏幕宽度自适应（旧实现沿用；页面里已有 meta viewport 时以页面为准）
        NSString *vp = @"width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no";
        NSString *vpJs = [NSString stringWithFormat:
            @"var m=document.querySelector('meta[name=viewport]');"
             "if(!m){m=document.createElement('meta');m.name='viewport';document.head.appendChild(m);}"
             "m.content='%@';", vp];
        WKUserScript *vpScript = [[WKUserScript alloc] initWithSource:vpJs
                                                        injectionTime:WKUserScriptInjectionTimeAtDocumentEnd
                                                     forMainFrameOnly:YES];
        [cfg.userContentController addUserScript:vpScript];
        [vpScript release];

        // 【关键】界面脚本以 DocumentStart 注入（早于页面脚本）。⚠️ 不能用
        // `stringWithFormat:`：脚本里含 `%`（CSS 宽度等）会被当格式说明符吃掉。
        if (initStr != nil) {
            WKUserScript *initScript = [[WKUserScript alloc] initWithSource:initStr
                                                              injectionTime:WKUserScriptInjectionTimeAtDocumentStart
                                                           forMainFrameOnly:YES];
            [cfg.userContentController addUserScript:initScript];
            [initScript release];
            [gInitJs release];
            gInitJs = [initStr copy];
        }

        gHandler = [[PhiraWebUIMessageHandler alloc] init];
        [cfg.userContentController addScriptMessageHandler:gHandler name:@"phira"];

        gScheme = [[PhiraWebUISchemeHandler alloc] init];
        [cfg setURLSchemeHandler:gScheme forURLScheme:@"phirc"];

        WKWebView *wv = [[WKWebView alloc] initWithFrame:root.view.bounds configuration:cfg];
        wv.autoresizingMask = UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight;
        wv.opaque = NO;
        wv.backgroundColor = UIColor.clearColor;
        wv.hidden = YES;
        wv.navigationDelegate = gHandler;
        // 移动端触控：关掉滚动视图的橡皮筋回弹（游戏内嵌 WebView 不该有 iOS 边缘拉扯）
        wv.scrollView.bounces = NO;
        wv.scrollView.alwaysBounceVertical = NO;
        wv.scrollView.alwaysBounceHorizontal = NO;
        // ⚠️ 不要学旧实现关 userInteractionEnabled —— 那是 loadHTMLString 时代的妥协；
        // 现在页面要完整交互（触点 / 输入框）。触摸被 WebView 吃掉是预期行为：
        // WebView 显示时界面归页面，隐藏时（GONE 同款）自然归还游戏视图。

        [cfg release];

        [root.view addSubview:wv];
        gWebView = wv;
        NSLog(@"[webui-ios] WKWebView 创建成功（隐藏，等待导航）");
    });
}

/// 导航到 `phirc://web/...`（对应 Rust 的 `WebCmd::Navigate`，URL 原样直传）。
void phira_webui_navigate(const char *url) {
    if (url == NULL) {
        return;
    }
    NSString *u = [NSString stringWithUTF8String:url];
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView == nil) {
            return;
        }
        NSURLRequest *req = [NSURLRequest requestWithURL:[NSURL URLWithString:u]];
        [gWebView loadRequest:req];
    });
}

/// 显示 / 隐藏（对应 Rust 的 `WebCmd::Visible`）。
void phira_webui_set_visible(int visible) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView == nil) {
            return;
        }
        gWebView.hidden = visible ? NO : YES;
    });
}

/// 在页面里执行一段 JS（对应 Rust 的 `WebCmd::Eval`；IPC 回包 / 推事件用）。
void phira_webui_eval(const char *js) {
    if (js == NULL) {
        return;
    }
    NSString *s = [NSString stringWithUTF8String:js];
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView != nil) {
            [gWebView evaluateJavaScript:s completionHandler:nil];
        }
    });
}

/// 释放全部资源（app 退出时调；不调也不泄漏到用户可感知的程度）。
void phira_webui_destroy(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [gWebView removeFromSuperview];
        [gWebView release];
        gWebView = nil;
        [gHandler release];
        gHandler = nil;
        [gScheme release];
        gScheme = nil;
        [gController release];
        gController = nil;
        [gInitJs release];
        gInitJs = nil;
        gIpcFn = NULL;
    });
}
