package org.flos.phira.modded;

import android.app.Activity;
import android.content.Context;
import android.graphics.Color;
import android.util.Log;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.JavascriptInterface;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.webkit.WebChromeClient;
import android.webkit.ConsoleMessage;
import android.widget.FrameLayout;

import java.io.ByteArrayInputStream;
import java.io.InputStream;
import java.util.HashMap;
import java.util.Map;

/**
 * 安卓端的 WebView 宿主（与 Windows 的 webui 模块对应）。
 *
 * 设计要点：
 *  * **默认隐藏**（GONE），只有 Rust 通过 JNI 请求时才显示 —— 不影响现有原生界面。
 *  * 页面来自 APK 内的 `assets/kaichang/`（build_apk.ps1 会打包），用
 *    `file:///android_asset/kaichang/xxx.html` 加载。
 *  * JS 桥：页面的 `window.ipc.postMessage(json)` 由 {@link Bridge#post(String)} 接住，
 *    转给 Rust（下一步接 JNI）；Rust 处理完回 `evaluateJavascript("window.__phircResolve(...)")`。
 *  * 贴图：`local-cover` / `respack/<idx>/<file>` 由 {@link #intercept} 直接读文件返回，
 *    与 Windows 的 `phirc://` 自定义协议等价（页面里仍用**相对路径**，不用改 HTML）。
 *
 * ⚠️ 这一版桥只落日志、暂不回包：等 Rust 侧 `webui_android` 接上再打开。
 */
public class WebViewHost {
    private static final String TAG = "PhiraWebView";
    /** 页面根（相对 APK assets）。 */
    private static final String ASSET_ROOT = "file:///android_asset/kaichang/";

    // ---- Rust（libphira.so）侧的回调。库在 MainActivity 里已加载，这里不会 UnsatisfiedLinkError；
    //      但为了万无一失，所有调用点都包 try-catch，绝不能让 WebView 拖垮游戏。 ----
    private static native void nativeRegisterHost(WebViewHost host);
    private static native void nativeIpc(String json);
    private static native void nativeReady();

    private static void safeNative(String what, Runnable r) {
        try {
            r.run();
        } catch (Throwable t) {
            Log.w(TAG, "native 调用失败（" + what + "）", t);
        }
    }

    private Activity activity;
    private FrameLayout container;
    private WebView webView;
    private boolean initialized = false;

    /** 懒初始化：第一次 show 时才建 WebView（省内存、也避免影响启动速度）。 */
    public void init(Activity activity, FrameLayout container) {
        if (initialized) return;
        this.activity = activity;
        this.container = container;
        initialized = true;

        webView = new WebView(activity);
        webView.setBackgroundColor(Color.BLACK);
        webView.setVisibility(View.GONE);

        WebSettings s = webView.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);          // tripsUide 的收藏卡片存在 localStorage
        s.setAllowFileAccess(true);
        s.setAllowContentAccess(true);
        s.setMediaPlaybackRequiresUserGesture(false);
        s.setSupportZoom(false);
        s.setBuiltInZoomControls(false);
        s.setTextZoom(100);
        s.setCacheMode(WebSettings.LOAD_NO_CACHE);
        // 不缩放：内容按视口宽度 1:1 铺满，避免「整体缩放 / 非均匀拉伸变形」。
        webView.setInitialScale(100);
        // 无 meta 的页面（开始页 index.html，冻结不可改）按设备宽度布局；
        // 有 meta（Setting / library / tripsUide 等写了 width=device-width）的页面
        // 仍尊重自身 meta。两者都不走 wide-viewport（980px）缩放。
        s.setUseWideViewPort(false);
        s.setLoadWithOverviewMode(false);

        // WebGL 背景光波需要硬件加速的合成层。WebView 覆盖在游戏 SurfaceView 之上时，
        // 显式指定 HARDWARE 层，避免 WebView 表面被按错误分辨率分配、再拉伸进横屏框
        // （「完全变形 / 光波变形」在强制横屏 + SurfaceView 叠层下的常见根因）。
        webView.setLayerType(View.LAYER_TYPE_HARDWARE, null);

        // —— 移动端触控/输入体验 ——
        // 1) 关掉边缘回弹的蓝色光亮（游戏内嵌 WebView 不该有系统滚动反馈）
        webView.setOverScrollMode(View.OVER_SCROLL_NEVER);
        // 2) 禁长按弹文本选择菜单（制谱器/曲库里长按 = 删音符等手势）
        webView.setLongClickable(false);
        webView.setHapticFeedbackEnabled(false);
        // 3) 隐藏滚动条（界面自己画了）
        webView.setVerticalScrollBarEnabled(false);
        webView.setHorizontalScrollBarEnabled(false);

        // 抓页面自己的 console（含 index.html 里 WebGL 失败的 console.error），方便排查。
        webView.setWebChromeClient(new WebChromeClient() {
            @Override
            public boolean onConsoleMessage(ConsoleMessage cm) {
                Log.i(TAG, "[page] " + cm.message());
                return true;
            }
        });

        webView.addJavascriptInterface(new Bridge(), "phiraAndroid");
        webView.setWebViewClient(new WebViewClient() {
            @Override
            public WebResourceResponse shouldInterceptRequest(WebView v, WebResourceRequest req) {
                return intercept(req);
            }

            @Override
            public void onPageStarted(WebView v, String url, android.graphics.Bitmap favicon) {
                super.onPageStarted(v, url, favicon);
                // 尽早注入桥脚本（页面自身脚本可能马上就要用 window.ipc）。
                // nativeReady → Rust 注入完整 init_scripts()，幂等，重复注入无害。
                safeNative("nativeReady(started)", () -> nativeReady());
            }

            @Override
            public void onPageFinished(WebView v, String url) {
                super.onPageFinished(v, url);
                // 兜底再注入一次（onPageStarted 时页面脚本可能跑得比 evaluateJavascript 快）。
                safeNative("nativeReady(finished)", () -> nativeReady());
                // 诊断：记录页面实际视口与 WebGL 可用性，便于排查「完全变形 / 光波没有」。
                // vw/vh 应当等于横屏屏宽/屏高（如 800x360 量级）；若它们是 980 或竖屏值，
                // 说明视口没按设备宽度生效，会整体缩放/拉伸。
                v.evaluateJavascript(
                    "(function(){try{var c=document.createElement('canvas');"
                    + "var gl=c.getContext('webgl2')||c.getContext('webgl');"
                    + "console.log('[phirc-diag] vw='+window.innerWidth+' vh='+window.innerHeight"
                    + "+' webgl='+(!!gl)+' url='+location.href);"
                    + "}catch(e){console.log('[phirc-diag] err '+e);}})();",
                    null);
                // Java 侧尺寸：若 view 宽高与 container 不一致（如 0 或竖屏值），
                // 说明 WebView 表面还没按真实横屏尺寸定稿 —— 「完全变形」的根因。
                Log.i(TAG, "[phirc-size] onPageFinished: view=" + v.getWidth() + "x" + v.getHeight()
                        + " container=" + (container != null
                            ? (container.getWidth() + "x" + container.getHeight()) : "null"));
                relayout();
            }
        });

        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT);
        container.addView(webView, lp);
        Log.i(TAG, "WebView 已创建（隐藏）");

        // 把实例交给 Rust（Rust 侧存全局引用，之后经 JNI 反向调 eval/setVisible/loadPage/refresh）
        final WebViewHost self = this;
        safeNative("nativeRegisterHost", () -> nativeRegisterHost(self));
    }

    /** 显示并加载页面（page 形如 "index.html"）。 */
    public void show(String page) {
        if (!initialized) return;
        activity.runOnUiThread(() -> {
            String url = ASSET_ROOT + (page == null || page.isEmpty() ? "index.html" : page);
            webView.loadUrl(url);
            webView.setVisibility(View.VISIBLE);
            webView.bringToFront();
            webView.requestFocus();
            relayout();
            // 再补一次：吃掉「盒子刚从 GONE 切到 VISIBLE、表面还没定稿」的瞬态拉伸
            webView.postDelayed(this::relayout, 120);
        });
    }

    /** 只显示，不重新加载（回到上次那一页）。 */
    public void showCurrent() {
        if (!initialized) return;
        activity.runOnUiThread(() -> {
            webView.setVisibility(View.VISIBLE);
            webView.bringToFront();
            webView.requestFocus();
        });
    }

    // ---- 以下三个方法由 Rust 经 JNI 调用（见 webui.rs 安卓段 forward_loop）----

    /** 显示 / 隐藏（对应 Rust 的 WebCmd::Visible）。 */
    public void setVisible(final boolean v) {
        if (!initialized) return;
        activity.runOnUiThread(() -> {
            if (v) {
                webView.setVisibility(View.VISIBLE);
                webView.bringToFront();
                webView.requestFocus();
                relayout();
                webView.postDelayed(this::relayout, 120);
            } else {
                webView.setVisibility(View.GONE);
                // 焦点还给游戏画面，否则触摸/键盘都被 WebView 吃掉
                if (container != null && container.getChildCount() > 0) {
                    container.getChildAt(0).requestFocus();
                }
            }
        });
    }

    /** 加载页面（对应 Rust 的 WebCmd::Navigate；Rust 侧已把 phirc:// 换成 file:///android_asset/）。 */
    public void loadPage(final String url) {
        if (!initialized) return;
        activity.runOnUiThread(() -> {
            webView.loadUrl(url);
            webView.bringToFront();
            webView.requestFocus();
        });
    }

    /** 重新断言可见性 + 焦点（对应 Rust 的 WebCmd::Refresh；全屏切换后用，幂等）。 */
    public void refresh() {
        if (!initialized) return;
        activity.runOnUiThread(() -> {
            if (webView.getVisibility() == View.VISIBLE) {
                webView.bringToFront();
                webView.requestFocus();
            }
        });
    }

    /**
     * 强制 WebView 按容器真实像素尺寸重新测量/布局，并让页面按真实视口重排。
     *
     * 背景：强制横屏（sensorLandscape）+ WebView 叠在游戏 SurfaceView 之上时，
     * WebView 的渲染表面常在「盒子还没定稿」的瞬态里被分配成错误尺寸
     * （0 宽、或竖屏宽）再被拉进横屏框，表现为整页「完全变形 / 光波变形 / 比例错」。
     * 这里显式按容器实际尺寸重设 LayoutParams + 直接 layout()，并注入 device-width 视口 +
     * 触发 resize，吃掉这个瞬态。
     */
    private void relayout() {
        if (webView == null || container == null) return;
        int w = container.getWidth();
        int h = container.getHeight();
        Log.i(TAG, "[phirc-size] relayout: container=" + w + "x" + h
                + " view=" + webView.getWidth() + "x" + webView.getHeight());
        if (w <= 0 || h <= 0) {
            // 容器还没量完（首帧）—— 下一帧再试
            webView.post(() -> relayout());
            return;
        }
        ViewGroup.LayoutParams lp = webView.getLayoutParams();
        if (lp == null) {
            lp = new FrameLayout.LayoutParams(w, h);
        } else {
            lp.width = w;
            lp.height = h;
        }
        webView.setLayoutParams(lp);
        // 直接落定视图矩形，确保 onSizeChanged 拿到正确尺寸 → 内部渲染表面随之 resize
        webView.layout(0, 0, w, h);
        webView.requestLayout();
        webView.forceLayout();
        // 让页面按真实视口重排（对齐冻结的 index.html 没有 viewport meta 的情况）
        webView.evaluateJavascript(
            "(function(){try{var m=document.querySelector('meta[name=viewport]');"
            + "if(!m){m=document.createElement('meta');m.name='viewport';"
            + "document.head.appendChild(m);}"
            + "m.content='width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no';"
            + "window.dispatchEvent(new Event('resize'));"
            + "}catch(e){console.log('[phirc-diag] relayout err '+e);}})();",
            null);
    }

    public void hide() {
        if (!initialized) return;
        activity.runOnUiThread(() -> {
            webView.setVisibility(View.GONE);
            // 焦点还给游戏画面，否则触摸/键盘都被 WebView 吃掉
            if (container != null && container.getChildCount() > 0) {
                container.getChildAt(0).requestFocus();
            }
        });
    }

    public boolean isVisible() {
        return initialized && webView != null && webView.getVisibility() == View.VISIBLE;
    }

    /** Rust → 页面：兑现 Promise / 执行任意脚本。 */
    public void eval(final String js) {
        if (!initialized) return;
        activity.runOnUiThread(() -> webView.evaluateJavascript(js, null));
    }

    /** 返回键：优先让 WebView 自己回退历史。 */
    public boolean goBack() {
        if (isVisible() && webView.canGoBack()) {
            webView.goBack();
            return true;
        }
        return false;
    }

    /**
     * 拦截子资源请求：曲绘与资源包贴图。
     * 与 Windows 的 `phirc://web/local-cover` / `respack/<idx>/<file>` 等价，
     * 但这里页面是 file:// 源，路径就是 assets 里的相对路径。
     */
    private WebResourceResponse intercept(WebResourceRequest req) {
        try {
            String url = req.getUrl().toString();
            String path = req.getUrl().getPath();
            if (path == null) return null;

            // kaichang/respack/<idx>/<file> → 读 APK assets 的 respack/<idx>/<file>
            // （贴图在游戏根 assets/respack/ 下，不在 kaichang/ 里 —— 页面以 kaichang/
            //   为源写相对路径，解析出来带 /kaichang 前缀，要去掉后再读）
            if (path.startsWith("/kaichang/respack/")) {
                String asset = path.substring("/kaichang".length()).substring(1);
                try {
                    InputStream in = activity.getAssets().open(asset);
                    Map<String, String> headers = new HashMap<>();
                    headers.put("Cache-Control", "no-cache");
                    return new WebResourceResponse(mimeOf(path), null, 200, "OK", headers, in);
                } catch (Exception ignored) {
                    // 没有这个资源包文件 → 走正常加载（404，页面降级显示）
                }
            }
            // 打包去重兜底：`kaichang/assets/<name>` 与根级同名文件在 APK 里只留根级
            // 一份（见 build_apk.ps1），页面两种路径都引用 —— assets/ 下 miss 时
            // 先试去掉前缀的根级文件（与 Rust serve_asset 的 strip-"assets/" 同款）。
            if (path.startsWith("/kaichang/assets/")) {
                String tail = path.substring("/kaichang/assets".length()); // 形如 /cmdysj.ttf
                try {
                    InputStream in = activity.getAssets().open("kaichang" + tail);
                    Map<String, String> headers = new HashMap<>();
                    headers.put("Cache-Control", "no-cache");
                    return new WebResourceResponse(mimeOf(path), null, 200, "OK", headers, in);
                } catch (Exception ignored) {
                    // 根级也没有 → 走正常加载（404）
                }
            }
            // local-cover?p=<base64(local_path)>[&big=1]：本地谱面的曲绘。
            // Windows 侧由 Rust 的 `load_local_cover` 生成缩略图走自定义协议；
            // 安卓是 file:// 源，这里直接读谱面目录里的曲绘原图（浏览器自行缩放）。
            if (path.contains("local-cover")) {
                String query = req.getUrl().getQuery();
                if (query != null) {
                    String p = null;
                    for (String kv : query.split("&")) {
                        if (kv.startsWith("p=")) p = kv.substring(2);
                    }
                    if (p != null) {
                        byte[] decoded = android.util.Base64.decode(p,
                                android.util.Base64.URL_SAFE | android.util.Base64.NO_WRAP);
                        String localPath = new String(decoded, java.nio.charset.StandardCharsets.UTF_8);
                        // 只放行曲库内的谱面路径，防止被当成任意文件读取器
                        boolean safe = (localPath.startsWith("custom/") || localPath.startsWith("download/"))
                                && !localPath.contains("..");
                        if (safe) {
                            java.io.File img = findIllustration(
                                    new java.io.File(new java.io.File(activity.getFilesDir(), "charts"), localPath));
                            if (img != null) {
                                InputStream in = new java.io.FileInputStream(img);
                                Map<String, String> headers = new HashMap<>();
                                headers.put("Cache-Control", "no-cache");
                                return new WebResourceResponse(mimeOf(img.getName()), null, 200, "OK", headers, in);
                            }
                        }
                        Log.w(TAG, "local-cover 未命中: " + localPath);
                    }
                }
                return null;
            }
        } catch (Exception e) {
            Log.w(TAG, "intercept 失败", e);
        }
        return null;
    }

    /** 在谱面目录里找曲绘文件（info.yml 的 illustration 字段，按常见名依次试）。 */
    private static java.io.File findIllustration(java.io.File dir) {
        if (!dir.isDirectory()) return null;
        String[] names = {
                "illustration.png", "illustration.jpg", "illustration.jpeg",
                "image.png", "image.jpg", "image.jpeg"
        };
        for (String n : names) {
            java.io.File f = new java.io.File(dir, n);
            if (f.isFile()) return f;
        }
        return null;
    }

    private static String mimeOf(String path) {
        String p = path.toLowerCase();
        if (p.endsWith(".png")) return "image/png";
        if (p.endsWith(".jpg") || p.endsWith(".jpeg")) return "image/jpeg";
        if (p.endsWith(".webp")) return "image/webp";
        if (p.endsWith(".ttf")) return "font/ttf";
        if (p.endsWith(".otf")) return "font/otf";
        if (p.endsWith(".js")) return "application/javascript";
        if (p.endsWith(".css")) return "text/css";
        if (p.endsWith(".json")) return "application/json";
        return "application/octet-stream";
    }

    /**
     * 页面的 JS 桥。协议与 Windows 完全一致：
     *   JS → `window.ipc.postMessage(JSON.stringify({ id, cmd, args }))`
     *   Rust → `window.__phircResolve(id, { ok, data | error })`
     *
     * 转交 Rust 的 `webui::handle_ipc`（与 Windows 同一条业务层）；
     * 回包由 Rust 经 JNI 调 {@link #eval} 注入 `window.__phircResolve(...)`。
     */
    public class Bridge {
        @JavascriptInterface
        public void post(String json) {
            Log.i(TAG, "IPC ← " + json);
            safeNative("nativeIpc", () -> nativeIpc(json));
        }
    }
}
