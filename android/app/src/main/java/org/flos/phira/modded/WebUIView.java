package org.flos.phira.modded;

import android.app.Activity;
import android.graphics.Color;
import android.os.Build;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.JavascriptInterface;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import java.io.ByteArrayInputStream;
import java.util.HashMap;
import java.util.Map;

/**
 * Android 侧的 WebView 宿主：把 {@code phira/ui/index.html}（那份逐字节照搬的原件）
 * 当成开始界面铺在 QuadSurface 之上，用 {@code window.AndroidBridge.postMessage(json)}
 * 把 JS 的 IPC 转回 Rust。
 *
 * <p>与 Windows 侧的 {@code webui_windows.rs}（wry 挂 HWND）职责一一对应：
 * <ul>
 *   <li>资源不走 file:// —— Android 上 file:// 会被判成不透明源，
 *       而且我们根本没法把 Rust 里 {@code include_bytes!} 的资源暴露成文件；
 *       所以改用 {@code https://webui.local/} + 内存资源表（{@link #putAsset}）。</li>
 *   <li>IPC：{@code AndroidBridge.postMessage} → {@link #nativeIpc} → Rust
 *       {@code webui::handle_ipc}，回包再走 {@code window.__phira_recv(...)}。</li>
 *   <li>显示 / 隐藏：直接切 View 可见性。游戏画面是 SurfaceView，
 *       WebView 叠在它上面时用 ops 控制，不销毁 WebView（避免每次重载页面）。</li>
 * </ul>
 *
 * <p>注意：Rust 里的资源表要在页面加载**之前**灌满，所以 {@link #loadHome()} 必须
 * 在 Rust 侧 {@code push_all_assets()} 之后调用。
 */
public final class WebUIView {

    /** Rust 侧 {@code include_bytes!} 进来的资源，由 {@link #ASSETS} 统一保管。 */

    /**
     * 内嵌资源表（**静态共享**，Rust 推什么、WebView 就取什么）。
     *
     * <p>⚠️ 原来这里是「先缓存在 PENDING，`create()` 时搬到实例表并置
     * `pendingFlushed`，之后忽略新资源」——那有**竞态**：
     * `webuiCreate` 是投到 UI 线程异步执行的，而 Rust 紧接着**同步**推资源。
     * 如果 UI 线程抢先跑完 `create()`，此时 PENDING 还是空的，
     * `pendingFlushed` 被置真 → **之后推来的 index.html 等资源全部被静默丢弃**
     * → 页面 404 → `onPageFinished` 照样触发 → Rust 以为就绪而交棒 → 空白/黑屏。
     *
     * <p>现在只有这一张静态表，不搬、不清、不设标志：Rust 推、WebView 读，
     * 两边都加锁，不存在时序问题（谁先谁后都对）。
     */
    private static final Map<String, byte[]> ASSETS = new HashMap<>();

    private final Activity activity;
    private WebView webView;
    private boolean created = false;
    private boolean visible = false;
    /** 页面是否已加载过（避免重复 loadUrl）。 */
    private boolean loaded = false;
    /** 首页是否已加载完成（`onPageFinished`）。Rust 收到 `webuiOnReady` 后才接管画面。 */
    private boolean ready = false;
    /** Rust 是否请求过显示（加载完成前不真的显示，避免露出空白页）。 */
    private boolean wantVisible = false;

    public WebUIView(Activity activity) {
        this.activity = activity;
    }

    private static String mimeOf(String path) {
        String p = path.toLowerCase();
        if (p.endsWith(".html")) return "text/html";
        if (p.endsWith(".js")) return "text/javascript";
        if (p.endsWith(".css")) return "text/css";
        if (p.endsWith(".png")) return "image/png";
        if (p.endsWith(".jpg") || p.endsWith(".jpeg")) return "image/jpeg";
        if (p.endsWith(".webp")) return "image/webp";
        if (p.endsWith(".ttf")) return "font/ttf";
        if (p.endsWith(".otf")) return "font/otf";
        if (p.endsWith(".json")) return "application/json";
        if (p.endsWith(".svg")) return "image/svg+xml";
        return "application/octet-stream";
    }

    /**
     * 把一份资源放进内存表（由 Rust 启动时调用，可能在任意线程）。
     *
     * <p>没有「先缓存后搬」那一套了 —— 就一张静态表，见 {@link #ASSETS} 的说明。
     */
    public static void putAssetStatic(String path, byte[] data) {
        if (path == null || data == null) return;
        String key = path.startsWith("/") ? path.substring(1) : path;
        synchronized (ASSETS) {
            ASSETS.put(key, data);
        }
    }

    /** 取一份资源（给 `shouldInterceptRequest` 用）。 */
    private static byte[] getAssetStatic(String path) {
        String key = path.startsWith("/") ? path.substring(1) : path;
        synchronized (ASSETS) {
            return ASSETS.get(key);
        }
    }

    /**
     * 在 UI 线程上创建 WebView 并加载首页。必须由 Rust 在**资源灌完之后**调用，
     * 且最终在本 Activity 的 UI 线程执行（Rust 从游戏线程调用 {@code runOnUiThread}）。
     */
    public void create() {
        if (created) return;
        created = true;

        final WebView wv = new WebView(activity);
        WebSettings s = wv.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        // 允许 https 页面里加载本地资源（字体 / 图片都走同一个自定义来源，一般用不上，
        // 但保留以防 WebView 把它当跨源）。
        s.setAllowFileAccess(false);
        s.setAllowContentAccess(false);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
            s.setMixedContentMode(WebSettings.MIXED_CONTENT_ALWAYS_ALLOW);
        }
        // 让页面按屏幕宽度自适应，不出现横向滚动条。
        s.setUseWideViewPort(true);
        s.setLoadWithOverviewMode(true);

        wv.setBackgroundColor(Color.TRANSPARENT);
        wv.addJavascriptInterface(new Bridge(), "AndroidBridge");

        wv.setWebViewClient(new WebViewClient() {
            @Override
            public WebResourceResponse shouldInterceptRequest(WebView view, WebResourceRequest request) {
                String host = request.getUrl().getHost();
                if (!"webui.local".equals(host)) return null;
                String path = request.getUrl().getPath();
                if (path == null) return null;
                if (path.startsWith("/")) path = path.substring(1);
                if (path.isEmpty()) path = "index.html";
                byte[] data = getAssetStatic(path);
                if (data == null) {
                    return new WebResourceResponse(
                            "text/plain", "utf-8", 404, "Not Found",
                            new HashMap<String, String>(),
                            new ByteArrayInputStream(new byte[0]));
                }
                Map<String, String> headers = new HashMap<>();
                headers.put("Cache-Control", "public, max-age=86400");
                return new WebResourceResponse(
                        mimeOf(path), null, 200, "OK", headers,
                        new ByteArrayInputStream(data));
            }

            @Override
            public void onPageFinished(WebView view, String url) {
                // 【关键】首页加载完成才通知 Rust「可以把界面交给 WebView 了」。
                //
                // 在此之前 Rust 一直在渲染原生 UI（WebView 是叠在上面的 View，
                // 初始 GONE）。如果提前通知，Rust 会立刻停止原生渲染，而 WebView
                // 还没内容 —— 屏幕就是纯黑的。
                ready = true;
                // 之前请求过显示才真正显出来，避免加载中途露出空白页
                if (wantVisible) {
                    view.setVisibility(View.VISIBLE);
                }
                try {
                    quad_native.QuadNative.webuiOnReady();
                } catch (Throwable t) {
                    CrashLog.error("webuiOnReady failed", t);
                }
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                // 站内导航交给 WebView，外部链接丢给系统浏览器。
                String host = request.getUrl().getHost();
                if ("webui.local".equals(host)) return false;
                try {
                    activity.startActivity(new android.content.Intent(
                            android.content.Intent.ACTION_VIEW, request.getUrl()));
                } catch (Exception ignored) {
                }
                return true;
            }
        });

        ViewGroup root = (ViewGroup) activity.findViewById(android.R.id.content);
        root.addView(wv, new ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT));
        wv.setVisibility(View.GONE);
        this.webView = wv;

        // 【关键】建完立刻加载首页 —— **不要**依赖外部调 `webuiLoadHome()`。
        //
        // 原来 Rust 的 `show()` 是「先推资源、再建 WebView」，而 `push_assets_once()`
        // 末尾就调 `webuiLoadHome()` —— 那一刻 `webUIView` 还是 null，调用被静默丢弃，
        // 于是 WebView 建出来后**从来没导航过任何页面**：永远 GONE、`onPageFinished`
        // 永不触发、`webuiOnReady` 永不回调 → 一直显示原生 UI。
        //
        // 放在这里就与外部调用顺序无关了：资源是加锁的静态表（推没推完都能读），
        // `loadHome()` 自身也有 `loaded` 幂等保护。
        loadHome();
    }

    /** 加载首页（幂等）。 */
    public void loadHome() {
        if (webView == null) return;
        if (loaded) return;
        loaded = true;
        webView.loadUrl("https://webui.local/index.html");
    }

    public void setVisible(boolean v) {
        this.visible = v;
        this.wantVisible = v;
        if (webView == null) return;
        // 首页还没加载完就先不显示 —— 否则会闪一下空白页/黑页
        final int vis = (v && ready) ? View.VISIBLE : View.GONE;
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (webView != null) webView.setVisibility(vis);
            }
        });
    }

    public boolean isVisible() {
        return visible;
    }

    /** 在页面里执行一段 JS（由 Rust 调用，等价于 wry 的 evaluate_script）。 */
    public void evaluate(final String js) {
        if (webView == null || js == null) return;
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (webView == null) return;
                try {
                    webView.evaluateJavascript(js, null);
                } catch (Exception ignored) {
                }
            }
        });
    }

    public void destroy() {
        if (webView == null) return;
        final WebView wv = webView;
        webView = null;
        activity.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    ViewGroup parent = (ViewGroup) wv.getParent();
                    if (parent != null) parent.removeView(wv);
                    wv.destroy();
                } catch (Exception ignored) {
                }
            }
        });
    }

    /**
     * JS 侧 {@code window.AndroidBridge.postMessage(json)} 的落点。
     *
     * <p>刻意声明成 {@code public static} 而不是私有内部类：`addJavascriptInterface`
     * 的桥接类要被 JS 反射调用，静态化可以避免隐式持有外部实例，也让可访问性更稳妥
     * （它本身就只用静态成员）。
     */
    public static final class Bridge {
        @JavascriptInterface
        public void postMessage(String json) {
            // 所有 JNI 符号必须挂在 quad_native.QuadNative 下（见该类顶部注释）。
            try {
                quad_native.QuadNative.webuiIpc(json);
            } catch (Throwable t) {
                CrashLog.error("webuiIpc failed", t);
            }
        }
    }
}
