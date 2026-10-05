// iOS WebView 开始页宿主（WKWebView）。
//
// 只被 phira/build.rs 在 iOS 目标下编译，其它平台不参与构建 —— 和 ios/motion.m 同一套路。
// 对外只暴露一组 C 函数，Rust 侧在 phira/src/webui_ios.rs 里声明同名 extern。
//
// # 为什么是 ObjC 而不是 objc2
//
// 工程里 iOS 侧已有先例（motion.m 包 CoreMotion），用 C 函数做边界最简单可靠；
// 而 `WKScriptMessageHandler` 是协议实现，用 objc2 写协议一致性既冗长又难调。
//
// # 资源怎么喂
//
// 不用自定义协议（那要写 `WKURLSchemeHandler`），也不用 `file://`
// （`index.html` 里 `texImage2D(img)` 会被 WebKit 判成跨源而抛 SecurityError）。
// 改成：Rust 侧先把整页内联成**单文件 HTML**（所有图片/字体变 base64 data URI），
// 这里用 `loadHTMLString:baseURL:nil` 一次性加载 —— 同源、无跨源问题。
//
// # 线程
//
// WKWebView 必须在主线程操作。Rust 从游戏线程调用，所以这里统一 `dispatch_async`
// 到主队列；函数本身立即返回（即发即忘），不阻塞游戏线程。
//
// # 内存管理
//
// 不启用 ARC（与 motion.m 一致），静态持有的对象手动 release。

#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>
#import <WebKit/WebKit.h>

/// Rust 侧提供的回调：JS 发来一条 IPC 消息（JSON 字符串）时调用。
typedef void (*phira_webui_ipc_fn)(const char *json);

static WKWebView *gWebView = nil;
static UIViewController *gController = nil;
static phira_webui_ipc_fn gIpcFn = NULL;
static int gLoaded = 0;

/// 接收页面 `window.webkit.messageHandlers.phira.postMessage(...)` 的消息。
///
/// 注意 `WKScriptMessage.body` 在传入 JavaScript 字符串时就是 `NSString`。
@interface PhiraWebUIMessageHandler : NSObject <WKScriptMessageHandler>
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

@end

static PhiraWebUIMessageHandler *gHandler = nil;

/// 取出当前 app 的根视图控制器（WKWebView 要挂到它的 view 上）。
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

/// 建 WebView 并加载内联好的单文件 HTML（必须在主线程调用）。
///
/// `html` 允许为 NULL（那就只把 WebView 建出来，之后再调 `phira_webui_load_html`）。
void phira_webui_create(const char *html, const char *init_js, phira_webui_ipc_fn ipc_fn) {
    gIpcFn = ipc_fn;
    NSString *htmlStr = (html != NULL) ? [NSString stringWithUTF8String:html] : nil;

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
        // 让页面按屏幕宽度自适应
        NSString *vp = @"width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no";
        NSString *js = [NSString stringWithFormat:
            @"var m=document.querySelector('meta[name=viewport]');"
             "if(!m){m=document.createElement('meta');m.name='viewport';document.head.appendChild(m);}"
             "m.content='%@';", vp];
        WKUserScript *us = [[WKUserScript alloc] initWithSource:js
                                                  injectionTime:WKUserScriptInjectionTimeAtDocumentEnd
                                               forMainFrameOnly:YES];
        [cfg.userContentController addUserScript:us];
        [us release];

        // 【关键】界面脚本（装 IPC 桥 + 绑定 5 个按钮）也**必须**以 WKUserScript 注入。
        //
        // ⚠️ 不能像原来那样在 Rust 侧 `create` 之后立刻 `evaluateJavaScript` ——
        // 那一刻 `loadHTMLString` 还没完成（它是异步的），脚本会落到 about:blank 上，
        // 真实页面一加载就把它覆盖掉。表现就是**开始界面一个按钮也点不动**
        // （和 Android 端当初缺注入是同一类问题）。
        //
        // ⚠️ 这里**不能**用 `stringWithFormat:`：脚本里含有 `%` 字样（CSS 宽度等），
        // 会被当成格式说明符。直接 `stringWithUTF8String:` 原样传入。
        if (init_js != NULL) {
            NSString *initStr = [NSString stringWithUTF8String:init_js];
            WKUserScript *usInit = [[WKUserScript alloc] initWithSource:initStr
                                                          injectionTime:WKUserScriptInjectionTimeAtDocumentEnd
                                                       forMainFrameOnly:YES];
            [cfg.userContentController addUserScript:usInit];
            [usInit release];
        }

        gHandler = [[PhiraWebUIMessageHandler alloc] init];
        [cfg.userContentController addScriptMessageHandler:gHandler name:@"phira"];

        WKWebView *wv = [[WKWebView alloc] initWithFrame:root.view.bounds configuration:cfg];
        wv.autoresizingMask = UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight;
        wv.opaque = NO;
        wv.backgroundColor = UIColor.clearColor;
        wv.hidden = YES;
        [cfg release];

        [root.view addSubview:wv];
        gWebView = wv;

        if (htmlStr != nil) {
            [wv loadHTMLString:htmlStr baseURL:nil];
            gLoaded = 1;
        }
        NSLog(@"[webui-ios] WKWebView 创建成功");
    });
}

/// 单独加载（或重新加载）内联 HTML。用于「创建时还没准备好 HTML」的场景。
void phira_webui_load_html(const char *html) {
    if (html == NULL) {
        return;
    }
    NSString *htmlStr = [NSString stringWithUTF8String:html];
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView == nil) {
            return;
        }
        [gWebView loadHTMLString:htmlStr baseURL:nil];
        gLoaded = 1;
    });
}

void phira_webui_set_visible(int visible) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView != nil) {
            gWebView.hidden = (visible == 0);
        }
    });
}

/// 在页面里执行一段 JS（等价于 Windows 侧 wry 的 evaluate_script）。
void phira_webui_eval(const char *js) {
    if (js == NULL) {
        return;
    }
    NSString *jsStr = [NSString stringWithUTF8String:js];
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView != nil) {
            [gWebView evaluateJavaScript:jsStr completionHandler:nil];
        }
    });
}

/// WebView 是否已经建好并加载过页面。
int phira_webui_ready(void) {
    return (gWebView != nil && gLoaded) ? 1 : 0;
}

void phira_webui_destroy(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gWebView != nil) {
            [gWebView.configuration.userContentController removeScriptMessageHandlerForName:@"phira"];
            [gWebView removeFromSuperview];
            [gWebView release];
            gWebView = nil;
        }
        if (gHandler != nil) {
            [gHandler release];
            gHandler = nil;
        }
        if (gController != nil) {
            [gController release];
            gController = nil;
        }
        gIpcFn = NULL;
        gLoaded = 0;
    });
}
