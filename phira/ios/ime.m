// iOS 软键盘桥（隐藏 UITextField）。
//
// 只被 phira/build.rs 在 iOS 目标下编译，其它平台不参与构建 —— 和 ios/motion.m 同一套路。
// 对外只暴露两个 C 函数（`phira_ime_show` / `phira_ime_set_text`），Rust 侧在
// phira/src/ime.rs 里声明同名 extern。
//
// # 为什么需要隐藏输入框
//
// `miniquad` 的 `show_keyboard` **只有 Android 实现了**，其余平台（含 iOS）走
// `native.rs` 的空实现。iOS 想弹键盘就得让某个 `UIResponder` `becomeFirstResponder`，
// 而最省事的就是一个不可见的 `UITextField`。文本本身**不留在**这里
// （每次回调后由 Rust 侧驱动同步），唯一真相在 Rust 侧的自绘输入框。
//
// # 线程
//
// UIKit 必须在主线程操作。Rust 从游戏线程调用，所以这里统一 `dispatch_async`
// 到主队列；函数本身立即返回（即发即忘），不阻塞游戏线程。
//
// # 内存管理
//
// 不启用 ARC（与 motion.m 一致），静态持有的对象手动 release。
//
// 历史：这些代码原来和 WKWebView 宿主一起放在 ios/webui.m 里。WebView 界面已整体
// 移除，但 iOS 的软键盘通道必须保留（否则自绘输入框在 iOS 上敲不进字），
// 于是单独拆成这个文件。

#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>

/// Rust 侧的两个回调（见 `phira/src/ime.rs`，`#[no_mangle] extern "C"` 导出）。
/// 软键盘每输入一个字符 / 退格一次，就回传一次。
extern void phira_ime_on_char(unsigned int c);
extern void phira_ime_on_backspace(void);
/// 由 Rust 在打开输入框时调用，把**已有文本**灌进隐藏框（见 `phira_ime_set_text`）。
extern void phira_ime_set_text(const char *text);

/// 隐藏的 `UITextField` —— 只为「唤出系统键盘」而存在。
static UITextField *gImeField = nil;

/// 隐藏输入框的委托（`UITextFieldDelegate`）。收字符 / 退格 / 回车都走它。
static id gImeDelegate = nil;

/// 只打前若干条字符日志，避免刷屏（诊断用，确认通路后可以删）。
static int gImeLogLeft = 32;

#pragma mark - 软键盘输入框子类（退格专用）

/// 专门为「**退格一定能收到**」而存在的子类。
///
/// ⚠️ 为什么必须覆写 `deleteBackward`：
/// 委托方法 `textField:shouldChangeCharactersInRange:replacementString:` **只在
/// 文本框里「有内容可删」时才会被调用**。所以按退格时 UIKit 会先判断
/// 「有没有东西可删」，没有就**根本不投递回调** —— 表现就是**只能输入、不能删除**。
///
/// `deleteBackward` 来自 `UIKeyInput`，**只要按了删除键就会被调用**，
/// 与文本是否为空无关，正是我们需要的入口。
@interface PhiraImeField : UITextField
@end

@implementation PhiraImeField

- (void)deleteBackward {
    NSLog(@"[ime-ios] ime backspace");
    phira_ime_on_backspace();
    // ⚠️ **必须调 super**：让隐藏框自身的文本也真的变短。
    //
    // 为什么：UIKit 的 `UITextField` 在文本为空时**可能根本不投递删除键**
    // （它先判断「有没有东西可删」）。如果这里不调 super，框会一直是空串，
    // 于是按退格时**连 `deleteBackward` 都不会被调用** —— 这正是
    // 「一个字也删不掉」的根因（只覆写却不调 super 修不好）。
    // 现在框里始终有与 Rust 侧同步的文本，退格永远有的删。
    [super deleteBackward];
}

@end

#pragma mark - 委托（UITextFieldDelegate）

@interface PhiraImeDelegate : NSObject <UITextFieldDelegate>
@end

@implementation PhiraImeDelegate

/// 软键盘输入/退格都走这里。
- (BOOL)textField:(UITextField *)textField
    shouldChangeCharactersInRange:(NSRange)range
                replacementString:(NSString *)string {
    (void)textField;
    // ⚠️ 退格**不在这里计数**（见 `PhiraImeField.deleteBackward`），否则会和它重复。
    //
    // ⚠️ 但 `range.length` 在**替换**时必须处理：它是这次替换被删掉的旧字符数。
    //    * 普通输入 → range.length == 0，不跑；
    //    * **粘贴覆盖选区**（尤其「全选 + 粘贴」）→ range.length == 被覆盖的长度，
    //      Rust 侧不先删掉的话旧字符会留着，结果是「旧的 + 新的」而不是「新的」。
    //    * 纯删除（退格）→ `string.length == 0`，而 `deleteBackward` 已经手动回调过一次，
    //      且 `[super deleteBackward]` 还会以 range.length=1 再走一遍这里 ——
    //      **所以必须用 `string.length > 0` 把纯删除挡掉，否则一次退格会删两个字符**
    //      （这就是「按一下退格少两格」的原因）。
    if (string.length > 0) {
        for (NSUInteger i = 0; i < range.length; i++) {
            phira_ime_on_backspace();
        }
    }
    for (NSUInteger i = 0; i < string.length; i++) {
        unsigned int c = (unsigned int)[string characterAtIndex:i];
        if (gImeLogLeft > 0) {
            gImeLogLeft--;
            NSLog(@"[ime-ios] ime char=%u (%C)", c, (unichar)c);
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

#pragma mark - iOS 软键盘桥（供 Rust 的 `phira::ime::show` 调用）

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
            // ⚠️ 光赋值 `.text` 不够：UIKit 不保证光标落在末尾（常见是**停在开头**）。
            // 光标若在开头，下一次输入 / 粘贴会插到最前面，而 Rust 侧永远往末尾追加
            // —— 两边立刻错位（表现就是「输入/粘贴进去的文字顺序不对」）。
            // 这里显式把光标推到末尾，和 Rust 侧「一律追加」的模型对齐。
            UITextPosition *end = gImeField.endOfDocument;
            gImeField.selectedTextRange = [gImeField textRangeFromPosition:end toPosition:end];
            NSLog(@"[ime-ios] ime set_text len=%lu", (unsigned long)gImeField.text.length);
        }
    });
}

/// 弹出 / 收起系统软键盘。由 Rust 侧 `phira_ime_show(i32)` 调用。
///
/// 必须在主线程操作 UIKit，所以这里 `dispatch_async` 到主队列。
void phira_ime_show(int show) {
    NSLog(@"[ime-ios] ime show 请求 show=%d", show);
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
                    NSLog(@"[ime-ios] ime: 找不到 key window，放弃（不弹键盘，避免卡死）");
                    return;
                }
                if (gImeDelegate == nil) {
                    gImeDelegate = [[PhiraImeDelegate alloc] init];
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
                tf.delegate = gImeDelegate;
                [win addSubview:tf];
                gImeField = tf;
                NSLog(@"[ime-ios] ime: 隐藏输入框已建在 window 上");
            }
            // 已经是第一响应者就别再调 —— 重复 becomeFirstResponder 会反复触发
            // 键盘的 appear/布局流程。
            if (![gImeField isFirstResponder]) {
                BOOL ok = [gImeField becomeFirstResponder];
                NSLog(@"[ime-ios] ime: becomeFirstResponder=%d", (int)ok);
            }
        } else if (gImeField != nil) {
            if ([gImeField isFirstResponder]) {
                [gImeField resignFirstResponder];
            }
            NSLog(@"[ime-ios] ime hide");
        }
    });
}
