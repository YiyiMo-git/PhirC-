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

/// 界面脚本（`START_PAGE_INIT_SCRIPT`）的副本，供 `didFinishNavigation` 兜底再注入。
/// MRC 下由 `create` 接管所有权（copy），`destroy` 释放。
static NSString *gInitJs = nil;

/// Rust 侧的两个回调（见 `phira/src/ime.rs`，`#[no_mangle] extern "C"` 导出）。
/// 软键盘每输入一个字符 / 退格一次，就回传一次。
extern void phira_ime_on_char(unsigned int c);
extern void phira_ime_on_backspace(void);
/// 由 Rust 在打开输入框时调用，把**已有文本**灌进隐藏框（见 `phira_ime_set_text`）。
extern void phira_ime_set_text(const char *text);

/// 隐藏的 `UITextField` —— 只为「唤出系统键盘」而存在。
///
/// 为什么需要它：`miniquad` 的 `show_keyboard` **只有 Android 实现了**，
/// 其余平台（含 iOS）走 `native.rs` 的空实现。iOS 想弹键盘就得让某个
/// `UIResponder` `becomeFirstResponder`，而最省事的就是一个不可见的 `UITextField`。
/// 文本本身**不留在**这里（每次回调后清空），唯一真相在 Rust 侧。
static UITextField *gImeField = nil;

/// 只打前若干条字符日志，避免刷屏（诊断用，确认通路后可以删）。
static int gImeLogLeft = 32;

/// 接收页面 `window.webkit.messageHandlers.phira.postMessage(...)` 的消息。
///
/// 注意 `WKScriptMessage.body` 在传入 JavaScript 字符串时就是 `NSString`。
@interface PhiraWebUIMessageHandler : NSObject <WKScriptMessageHandler, WKNavigationDelegate, UITextFieldDelegate>
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

/// 页面加载完成后：
///   1. **兜底再注入一次界面脚本** —— 脚本自身是幂等的（`__phiraStartPageBridgeInstalled`），
///      所以重复执行安全。`WKUserScript` 理论上够用，但实测「按钮一个都点不动」时它没生效，
///      这里补一次，时机与 **Android 端的 `onPageFinished` 注入完全一致**。
///   2. 跑一段**自检**并把结果打进日志 —— 桥有没有装上、messageHandler 在不在、
///      按钮有几个，一眼就能看出断在哪一环（不用再猜）。
- (void)webView:(WKWebView *)webView didFinishNavigation:(WKNavigation *)navigation {
    (void)navigation;
    if (gInitJs != nil) {
        [webView evaluateJavaScript:gInitJs completionHandler:nil];
    }
    NSString *probe =
        @"(function(){"
         @"var b=!!window.__phiraStartPageBridgeInstalled;"
         @"var hid=!!window.__phiraSetHidden;"
         @"var hm=!!(window.webkit&&window.webkit.messageHandlers&&window.webkit.messageHandlers.phira);"
         @"var n=document.querySelectorAll('[id^=\"btn-\"]').length;"
         @"return 'bridge='+b+' setHidden='+hid+' handler='+hm+' btns='+n;"
         @"})()";
    [webView evaluateJavaScript:probe completionHandler:^(id result, NSError *error) {
        NSLog(@"[webui-ios] 自检 %@ error=%@", result, error);
    }];
}

#pragma mark - 软键盘（UITextFieldDelegate）

/// 软键盘输入/退格都走这里。
///
/// 注意 `return NO` + 清空 `text`：**不让输入框自己累积文本** —— 唯一真相在 Rust
/// 侧（自绘输入框的 `edit.text`），这里只当「按键事件源」。若让它累积，两边状态
/// 会不一致（Rust 删一个字、UITextField 里还留着）。
- (BOOL)textField:(UITextField *)textField
    shouldChangeCharactersInRange:(NSRange)range
                replacementString:(NSString *)string {
    (void)textField;
    // ⚠️ 退格**不在这里处理**（见 `PhiraImeField.deleteBackward`），否则会和它重复计数。
    for (NSUInteger i = 0; i < string.length; i++) {
        unsigned int c = (unsigned int)[string characterAtIndex:i];
        if (gImeLogLeft > 0) {
            gImeLogLeft--;
            NSLog(@"[webui-ios] ime char=%u (%C)", c, (unichar)c);
        }
        phira_ime_on_char(c);
    }
    // ⭐ 返回 **YES**：让隐藏框**真的累积文本**。
    //
    // 最初写成 `return NO`（框恒空、唯一真相在 Rust），结果 UIKit 认为「没东西可删」，
    // **退格键根本不会投递**。现在框里的文本与 Rust 侧一一对应（都由同一批事件驱动），
    // 退格永远有的删；顺带 iOS 的长按全选 / 粘贴也能用了。
    return YES;
}

/// 软键盘右下角「完成」= 回车 → 自绘输入框据此确认。
- (BOOL)textFieldShouldReturn:(UITextField *)textField {
    (void)textField;
    phira_ime_on_char(0x0D);
    return YES;
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
             @"if(!m){m=document.createElement('meta');m.name='viewport';document.head.appendChild(m);}"
             @"m.content='%@';", vp];
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
            // 存一份给 didFinishNavigation 兜底用（MRC：先放旧的，再 copy 新的）
            [gInitJs release];
            gInitJs = [initStr copy];
        }

        gHandler = [[PhiraWebUIMessageHandler alloc] init];
        [cfg.userContentController addScriptMessageHandler:gHandler name:@"phira"];

        WKWebView *wv = [[WKWebView alloc] initWithFrame:root.view.bounds configuration:cfg];
        wv.autoresizingMask = UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight;
        wv.opaque = NO;
        wv.backgroundColor = UIColor.clearColor;
        wv.hidden = YES;
        wv.navigationDelegate = gHandler;

        // 【关键】关掉 WebView 的交互 —— 让触摸**穿透到下面的游戏视图**。
        //
        // 为什么：iOS 上这个 WKWebView 的 JS 注入怎么都不生效（`WKUserScript` 和
        // `didFinishNavigation` 里再 `evaluateJavaScript` 都试过），`index.html` 里
        // 5 个按钮的 `addEventListener` 永远绑不上，点了完全没有反应。
        //
        // 我们改成不依赖 JS：`userInteractionEnabled = NO` 之后，
        // `hitTest:withEvent:` 对本视图返回 nil，触摸落到它下面那一层 ——
        // 也就是 miniquad 的 `QuadView`（本 WebView 就是 `addSubview` 到 root.view
        // 上的，而 root.view 正是 `QuadView`，见 `phira_root_view_controller`：
        // miniquad 把 `QuadView` 设成了 window 的 rootViewController.view）。
        // 游戏线程照常收到触摸，Rust 侧用矩形命中按钮 —— 见
        // `phira/src/webui_hit.rs` 与 `MainScene::touch`。
        //
        // 代价：页面自己的交互（双击左侧曲绘换图、文件选择）一并没有了。
        // 那本来就是页面自带的小功能，换「5 个按钮至少能用」完全值得。
        wv.userInteractionEnabled = NO;

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

#pragma mark - 软键盘输入框子类（退格专用）

/// 专门为「**退格一定能收到**」而存在的子类。
///
/// ⚠️ 为什么必须覆写 `deleteBackward`：
/// 委托方法 `textField:shouldChangeCharactersInRange:replacementString:` **只在
/// 文本框里「有内容可删」时才会被调用**。我们的隐藏输入框**始终是空的**
/// （`shouldChange` 里返回 `NO`，从不把字符写进去），所以按退格时 UIKit 认为
/// 「没东西可删」，**回调根本不触发** —— 表现就是**只能输入、不能删除**（用户实测）。
///
/// `deleteBackward` 来自 `UIKeyInput`，**只要按了删除键就会被调用**，
/// 与文本是否为空无关，正是我们需要的入口。
///
/// 故意**不调 `super`**：文本本来就是空的，没有需要它做的事；调了反而可能
/// 把状态搞乱。
@interface PhiraImeField : UITextField
@end

@implementation PhiraImeField

- (void)deleteBackward {
    NSLog(@"[webui-ios] ime backspace");
    phira_ime_on_backspace();
    // ⚠️ **必须调 super**：让隐藏框自身的文本也真的变短。
    //
    // 为什么：UIKit 的 `UITextField` 在文本为空时**可能根本不投递删除键**
    // （它先判断「有没有东西可删」）。如果这里不调 super，框会一直是空串，
    // 于是按退格时**连 `deleteBackward` 都不会被调用** —— 这正是
    // 「一个字也删不掉」的根因（我上一轮只覆写却没调 super，所以没修好）。
    // 现在框里始终有与 Rust 侧同步的文本，退格永远有的删。
    [super deleteBackward];
}

@end

#pragma mark - iOS 软键盘桥（供 Rust 的 `phira::ime::show` 调用）

/// 弹出 / 收起系统软键盘。由 Rust 侧 `phira_ime_show(i32)` 调用。
///
/// 必须在主线程操作 UIKit，所以这里 `dispatch_async` 到主队列。
/// 把**已有文本**灌进隐藏输入框。由 Rust 打开输入框时调用（带入 `default`）。
///
/// ⚠️ 必须做这件事：Rust 侧一开场就有文本（例如连击文字原有的 `RECALL`），
/// 而隐藏框若是空的，用户按退格时 UIKit 又认为「没东西可删」→ **删不掉原有内容**。
/// 两边都从同一份文本起步，才能一一对应。
void phira_ime_set_text(const char *text) {
    if (text == NULL) {
        return;
    }
    NSString *s = [NSString stringWithUTF8String:text];
    dispatch_async(dispatch_get_main_queue(), ^{
        if (gImeField != nil) {
            gImeField.text = (s != nil) ? s : @"";
            NSLog(@"[webui-ios] ime set_text len=%lu", (unsigned long)gImeField.text.length);
        }
    });
}

void phira_ime_show(int show) {
    NSLog(@"[webui-ios] ime show 请求 show=%d", show);
    dispatch_async(dispatch_get_main_queue(), ^{
        if (show) {
            if (gImeField == nil) {
                // ⚠️ 关键：隐藏输入框**挂到 UIWindow 上**，不要挂到根视图。
                //
                // 根视图就是 miniquad 的 `GLKView`。在 GLKView 里塞子视图 + 弹键盘，
                // 会让键盘动画期间反复触发 `layoutSubviews` → 重建 framebuffer，
                // 是出名的**布局递归 / 卡死**陷阱（用户实测「点击输入框当场卡死」）。
                // window 不是 GL 视图，挂它上面就没有这个问题。
                UIWindow *win = nil;
                for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
                    if (![scene isKindOfClass:[UIWindowScene class]]) {
                        continue;
                    }
                    for (UIWindow *w in ((UIWindowScene *)scene).windows) {
                        if (w.isKeyWindow) {
                            win = w;
                            break;
                        }
                    }
                    if (win != nil) {
                        break;
                    }
                }
                if (win == nil) {
                    NSLog(@"[webui-ios] ime: 找不到 key window，放弃（不弹键盘，避免卡死）");
                    return;
                }
                // 用子类：退格走 `deleteBackward`（见上面的说明）
                UITextField *tf = [[PhiraImeField alloc] initWithFrame:CGRectMake(0.0, -100.0, 1.0, 1.0)];
                // 不参与 autoresizing：否则键盘出现时的尺寸变化会把它一起重排。
                tf.autoresizingMask = UIViewAutoresizingNone;
                tf.autocorrectionType = UITextAutocorrectionTypeNo;
                tf.autocapitalizationType = UITextAutocapitalizationTypeNone;
                tf.spellCheckingType = UITextSpellCheckingTypeNo;
                tf.keyboardType = UIKeyboardTypeASCIICapable;
                tf.returnKeyType = UIReturnKeyDone;
                if (gHandler == nil) {
                    NSLog(@"[webui-ios] ime: gHandler 为空！委托设不上，字符回传不会发生");
                }
                tf.delegate = gHandler;
                [win addSubview:tf];
                gImeField = tf;
                NSLog(@"[webui-ios] ime: 隐藏输入框已建在 window 上");
            }
            // 已经是第一响应者就别再调 —— 重复 becomeFirstResponder 会反复触发
            // 键盘的 appear/布局流程。
            if (![gImeField isFirstResponder]) {
                BOOL ok = [gImeField becomeFirstResponder];
                NSLog(@"[webui-ios] ime: becomeFirstResponder=%d", (int)ok);
            }
        } else if (gImeField != nil) {
            if ([gImeField isFirstResponder]) {
                [gImeField resignFirstResponder];
            }
            NSLog(@"[webui-ios] ime hide");
        }
    });
}
