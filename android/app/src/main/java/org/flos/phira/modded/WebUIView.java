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

    /** Rust 侧 {@code include_bytes!} 进来的资源，键是相对路径（如 {@code assets/3.png}）。 */
    private final Map<String, byte[]> assets = new HashMap<>();

    /**
     * 还没建立 WebView 时先存在这里的资源。
     *
     * <p>Rust 是**同步**把资源推过来的（启动早期），而 WebView 必须在 UI 线程建，
     * 建好之前 {@link #putAsset} 无处可放。所以先落在这张静态缓冲表里，
     * {@link #create()} 时再一次性搬进实例表。这样 Rust 那边不用关心时序。
     */
    private static final Map<String, byte[]> PENDING = new HashMap<>();
    private static boolean pendingFlushed = false;

    private final Activity activity;
    private WebView webView;
    private boolean created = false;
    private boolean visible = false;
    /** 页面是否已加载过（避免重复 loadUrl）。 */
    private boolean loaded = false;

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
     * 把一份资源放进内存表（由 Rust 启动时调用）。
     *
     * <p>WebView 还没建好时先进静态缓冲表，{@link #create()} 会搬过去。
     * 这个方法由 Rust 在任何线程调用，所以两张表都要按「只增不改」的方式用，
     * 避免和 UI 线程的 {@link #create()} 抢。
     */
    public static void putAssetStatic(String path, byte[] data) {
        if (path == null || data == null) return;
        String key = path.startsWith("/") ? path.substring(1) : path;
        synchronized (PENDING) {
            if (pendingFlushed) return; // 已经搬过了，忽略（页面早已加载）
            PENDING.put(key, data);
        }
    }

    private void putAssetLocal(String path, byte[] data) {
        if (path == null || data == null) return;
        assets.put(path.startsWith("/") ? path.substring(1) : path, data);
    }

    /** 把缓冲表里的资源搬进实例表（只在 UI 线程、且只做一次）。 */
    private void flushPending() {
        synchronized (PENDING) {
            if (!pendingFlushed) {
                for (Map.Entry<String, byte[]> e : PENDING.entrySet()) {
                    assets.put(e.getKey(), e.getValue());
                }
                PENDING.clear();
                pendingFlushed = true;
            }
        }
    }

    /**
     * 在 UI 线程上创建 WebView 并加载首页。必须由 Rust 在**资源灌完之后**调用，
     * 且最终在本 Activity 的 UI 线程执行（Rust 从游戏线程调用 {@code runOnUiThread}）。
     */
    public void create() {
        if (created) return;
        created = true;
        // Rust 在 WebView 建好之前推过来的资源，这里一次性搬进来
        flushPending();

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
                byte[] data = assets.get(path);
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
        if (webView == null) return;
        final int vis = v ? View.VISIBLE : View.GONE;
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

    /** JS 侧 {@code window.AndroidBridge.postMessage(json)} 的落点。 */
    private final class Bridge {
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
