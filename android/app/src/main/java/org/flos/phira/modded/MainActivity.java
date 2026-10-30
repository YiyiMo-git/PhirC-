package org.flos.phira.modded;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.hardware.Sensor;
import android.hardware.SensorEvent;
import android.hardware.SensorEventListener;
import android.hardware.SensorManager;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
import android.util.DisplayMetrics;
import android.util.Log;
import android.view.Display;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;

import moe.mivik.inputbox.InputBox;
import quad_native.QuadNative;

/**
 * Phira（改版）的 Android 外壳。
 *
 * 契约（对着 miniquad 的 java/ 模板、src/native/android.rs 和 phira 自己导出的
 * Java_quad_1native_QuadNative_* 核过）：
 *
 *  Java → Rust（QuadNative 里的 native 方法）：
 *    - miniquad 的 13 个回调（activityOn* / surfaceOn* / initializeContext / releaseContext）
 *    - phira 自己的：initializeEnvironment / setDataPath / setTempDir / setDpi /
 *      setChosenFile / markImport / markImportRespack / setInputText / processExportFd /
 *      prprActivityOn* / startApp
 *    少一个或参数个数不对，Rust 侧不是 NULL 就是垃圾值。
 *
 *  Rust → Java（Rust 用 GetMethodID 按名字找，找不到就是 NULL 然后 abort）：
 *    - finish()         Activity 自带
 *    - getAssets()      Activity 自带
 *    - setFullScreen(boolean)    见下
 *    - showKeyboard(boolean)     见下
 *    - copy(String)              见下（剪贴板）
 *    - openUrl(String)           见下（prpr::ext::open_url，点链接用）
 *    - showExportDialog(String)  见下（谱面导出）
 *    - deleteUri(Uri)            见下（导出结束后清理 SAF 文档）
 *    - chooseFile()              见下（选文件导入）
 *
 *  启动顺序：initializeContext（把 Activity 绑到 ndk_context，读 APK 内 assets 靠它）
 *  → setDataPath / setTempDir / setDpi → initializeEnvironment（inputbox 后端）
 *  → 建 SurfaceView → activityOnCreate。
 */
class QuadSurface extends SurfaceView implements View.OnTouchListener, View.OnKeyListener, SurfaceHolder.Callback {
    public QuadSurface(android.content.Context context) {
        super(context);
        getHolder().addCallback(this);
        setFocusable(true);
        setFocusableInTouchMode(true);
        requestFocus();
        setOnTouchListener(this);
        setOnKeyListener(this);
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        CrashLog.stage("surfaceCreated");
        QuadNative.surfaceOnSurfaceCreated(holder.getSurface());
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        CrashLog.stage("surfaceDestroyed");
        QuadNative.surfaceOnSurfaceDestroyed(holder.getSurface());
    }

    /**
     * 向 Surface 投帧率票。Android 11+ 用 `Surface.setFrameRate`（FILL 语义：
     * 系统挑不超过它的最高档），更老的系统只在窗口层投票（见 applyRequestedFrameRate）。
     */
    void setTargetFrameRate(float rate, boolean force, boolean focused) {
        try {
            Surface surface = getHolder().getSurface();
            if (!surface.isValid()) {
                return;
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                surface.setFrameRate(rate, Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE,
                        focused ? Surface.CHANGE_FRAME_RATE_ALWAYS : Surface.CHANGE_FRAME_RATE_ONLY_IF_SEAMLESS);
            }
        } catch (Throwable t) {
            CrashLog.stage("[refresh] surface 投票失败：" + t);
        }
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
        // 游戏线程会一直等这条消息才开始建 EGL 上下文，记一笔方便确认「到底有没有画面」
        CrashLog.stage("surfaceChanged " + width + "x" + height);
        QuadNative.surfaceOnSurfaceChanged(holder.getSurface(), width, height);
    }

    /**
     * 让系统输入法有地方「提交文本」。
     *
     * ⚠️ 这是**移动端能打字的关键**：`SurfaceView` 默认不提供输入连接，于是
     * `InputMethodManager.showSoftInput(view, 0)` 之后输入法虽然弹出来了，
     * `commitText` 却无处可去 —— 字符永远到不了原生层。表现就是
     * 「键盘弹出来了、但一个字都打不进去」。
     *
     * 所有软键盘输入都走 Rust 的线程安全 `IME_BUFFER`（和 iOS 同一条路），避免数据竞争：
     *   * 文本 → `QuadNative.imeChar(int)`（UTF-32 码点，直接进缓冲区）；
     *   * 删除 → `QuadNative.imeBackspace(int)`（删除个数计数通道）；
     *   * 「完成」→ `QuadNative.imeConfirm()`（确认事件）。
     * 这三者都不碰 macroquad 的裸键 / 字符队列（IME 线程 push / 渲染线程 pop 无同步，
     * 是「输入框有时把整个软件卡死」的根因）。
     */
    private static boolean gImeLogged = false;

    @Override
    public InputConnection onCreateInputConnection(android.view.inputmethod.EditorInfo outAttrs) {
        // ⚠️ **不要**加 `TYPE_TEXT_VARIATION_VISIBLE_PASSWORD`！
        //
        // 之前这里是 `TYPE_CLASS_TEXT | TYPE_TEXT_FLAG_NO_SUGGESTIONS
        // | TYPE_TEXT_VARIATION_VISIBLE_PASSWORD` —— 用 VISIBLE_PASSWORD 是为了
        // 「文字可见 + 压掉联想和自动纠错」，但它是**密码变体**，很多输入法（搜狗、
        // 各家安全输入法）看到它就直接切到**密码安全键盘**：没有候选栏、没有剪贴板、
        // 布局也变样。用户报的「输入框默认弹出的是安全键盘」就是它。
        //
        // 正确做法：用普通的 `TYPE_CLASS_TEXT`，靠这两个 flag 达到原本的目的 ——
        //   * TYPE_TEXT_FLAG_NO_SUGGESTIONS        —— 不出候选/联想（inputType 标志）
        //   * IME_FLAG_NO_PERSONALIZED_LEARNING    —— 不让输入法把输入学进用户词库
        // 两者都不改变键盘形态，弹出的是正常键盘。
        //
        // ⚠️ 这两个标志**不在同一个桶里**：
        //   - `TYPE_TEXT_FLAG_NO_SUGGESTIONS` 是 `android.text.InputType` 的 inputType 位；
        //   - `IME_FLAG_NO_PERSONALIZED_LEARNING` 是 `android.view.inputmethod.EditorInfo`
        //     的 **imeOptions** 位（API 26+，minSdk 26 满足）。
        //   之前误写成 `InputType.TYPE_TEXT_FLAG_NO_PERSONALIZED_LEARNING`（这个常量
        //   **根本不存在**）并 OR 进 inputType，导致 javac 直接编译失败 ——
        //   见 `build_apk.log` 里 `:app:compileReleaseJavaWithJavac FAILED`。
        //
        // 真正的密码字段由自绘输入框自己打成 `*`（`input_dialog.rs`），不依赖输入法变体；
        // 旧的 `moe/mivik/inputbox/InputBox.java` 也自己按模式设了正确的 inputType。
        outAttrs.inputType = android.text.InputType.TYPE_CLASS_TEXT
                | android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
        outAttrs.imeOptions = android.view.inputmethod.EditorInfo.IME_ACTION_DONE
                | android.view.inputmethod.EditorInfo.IME_FLAG_NO_FULLSCREEN
                | android.view.inputmethod.EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING;
        if (!gImeLogged) {
            gImeLogged = true;
            CrashLog.stage("onCreateInputConnection: 输入法已连接（可以打字了）");
        }
        return new android.view.inputmethod.BaseInputConnection(this, false) {
            @Override
            public boolean commitText(CharSequence text, int newCursorPosition) {
                if (text != null) {
                    // ⚠️ 必须用**码点**遍历，不能直接 `charAt(i)`：
                    // `charAt` 返回 UTF-16 code unit，emoji / 生僻字是代理对，
                    // 会被拆成两个孤立的代理值发给 Rust，拼不出原字符。
                    // 走 `QuadNative.imeChar(int)` —— 它把 UTF-32 码点**直接**压进
                    // Rust 的线程安全 `IME_BUFFER`（和退格同一个互斥锁），不再碰
                    // macroquad 的裸字符队列（那是「输入框有时把整个软件卡死」的根因：
                    // IME 线程 push / 渲染线程 pop 之间无同步，会数据竞争）。
                    String s = text.toString();
                    for (int i = 0; i < s.length(); ) {
                        int cp = s.codePointAt(i);
                        QuadNative.imeChar(cp);
                        i += Character.charCount(cp);
                    }
                }
                return true;
            }

            @Override
            public boolean setComposingText(CharSequence text, int newCursorPosition) {
                // ⚠️ **不要**转成 commitText：拼音/中文输入时「组合中」的文本会随每次按键
                // 回调一次，转发出去就会把同一个字重复插入。这里只吃下、不插入，
                // 等输入法真正 commitText 时再插入。
                return true;
            }

            @Override
            public boolean finishComposingText() {
                return true;
            }

            @Override
            public boolean deleteSurroundingText(int beforeLength, int afterLength) {
                // ⚠️ 一次把「删除个数」传过去，**不要**在这里连发 N 次 DEL 键事件：
                // 那 N 个事件会在同一帧入队，而 macroquad 的 `keys_pressed` 是 `HashSet`，
                // 同一个键在同一帧多次按下会坍缩成一次 —— 结果只删掉 1 个字符，
                // 「选中一段文字后粘贴 / 输入」时旧字符会残留。
                // 走 `imeBackspace` 计数通道，个数与顺序都保真（见 QuadNative.imeBackspace）。
                if (beforeLength > 0) {
                    QuadNative.imeBackspace(beforeLength);
                }
                // afterLength（向后删）在引擎当前「一律尾部追加 / 尾部退格」的文本模型里
                // 表达不了，输入法也极少用到；这里明确忽略，而不是假装处理。
                return true;
            }

            @Override
            public boolean sendKeyEvent(KeyEvent event) {
                if (event.getKeyCode() == KeyEvent.KEYCODE_DEL) {
                    if (event.getAction() == KeyEvent.ACTION_DOWN) {
                        QuadNative.surfaceOnKeyDown(KeyEvent.KEYCODE_DEL);
                    } else {
                        QuadNative.surfaceOnKeyUp(KeyEvent.KEYCODE_DEL);
                    }
                    return true;
                }
                return super.sendKeyEvent(event);
            }

            @Override
            public boolean performEditorAction(int actionCode) {
                // 输入法右下角那个「完成 / 前往」按钮 = 确认游戏内输入框。
                // ⚠️ 不再走 `surfaceOnKeyDown/Up(KEYCODE_ENTER)`：那个事件照旧进
                // macroquad 的裸键队列，有 IME 线程 / 渲染线程竞争（同卡死根因）。
                // 改走 `QuadNative.imeConfirm()` —— 它把「确认」压进线程安全的
                // `IME_BUFFER`，和字符、退格同一条安全路径。语义 = 确定，等价于面板「确定」按钮。
                QuadNative.imeConfirm();
                return true;
            }
        };
    }

    @Override
    public boolean onTouch(View v, MotionEvent event) {
        final int pointerCount = event.getPointerCount();
        final long time = event.getEventTime();
        switch (event.getActionMasked()) {
            case MotionEvent.ACTION_MOVE:
                for (int i = 0; i < pointerCount; i++) {
                    QuadNative.surfaceOnTouch(event.getPointerId(i), 0, event.getX(i), event.getY(i), time);
                }
                break;
            case MotionEvent.ACTION_UP:
                QuadNative.surfaceOnTouch(event.getPointerId(0), 1, event.getX(0), event.getY(0), time);
                break;
            case MotionEvent.ACTION_DOWN:
                QuadNative.surfaceOnTouch(event.getPointerId(0), 2, event.getX(0), event.getY(0), time);
                break;
            case MotionEvent.ACTION_POINTER_UP: {
                final int i = event.getActionIndex();
                QuadNative.surfaceOnTouch(event.getPointerId(i), 1, event.getX(i), event.getY(i), time);
                break;
            }
            case MotionEvent.ACTION_POINTER_DOWN: {
                final int i = event.getActionIndex();
                QuadNative.surfaceOnTouch(event.getPointerId(i), 2, event.getX(i), event.getY(i), time);
                break;
            }
            case MotionEvent.ACTION_CANCEL:
                for (int i = 0; i < pointerCount; i++) {
                    QuadNative.surfaceOnTouch(event.getPointerId(i), 3, event.getX(i), event.getY(i), time);
                }
                break;
            default:
                break;
        }
        return true;
    }

    // getCharacters 已废弃，但非拉丁输入时 KeyEvent 的 keyCode 常常是 0，只能靠它拿字符
    @SuppressWarnings("deprecation")
    @Override
    public boolean onKey(View v, int keyCode, KeyEvent event) {
        // ⚠️ 音量键必须**放行**（return false），不能吞。
        // 这个方法末尾是 `return true`，也就是把所有按键都消费掉；音量键被这个 View
        // 消费之后，系统就没机会去调媒体音量了 —— 表现就是「手机侧边音量键对游戏完全无效」。
        // 注意必须是 return false 而不是 super：要让事件继续往系统传。
        if (keyCode == KeyEvent.KEYCODE_VOLUME_UP
                || keyCode == KeyEvent.KEYCODE_VOLUME_DOWN
                || keyCode == KeyEvent.KEYCODE_VOLUME_MUTE) {
            return false;
        }
        if (event.getAction() == KeyEvent.ACTION_DOWN && keyCode != 0) {
            QuadNative.surfaceOnKeyDown(keyCode);
        }
        if (event.getAction() == KeyEvent.ACTION_UP && keyCode != 0) {
            QuadNative.surfaceOnKeyUp(keyCode);
        }
        if (event.getAction() == KeyEvent.ACTION_UP || event.getAction() == KeyEvent.ACTION_MULTIPLE) {
            int character = event.getUnicodeChar();
            if (character == 0) {
                String characters = event.getCharacters();
                if (characters != null && characters.length() > 0) {
                    character = characters.charAt(0);
                }
            }
            if (character != 0) {
                QuadNative.surfaceOnCharacter(character);
            }
        }
        return true;
    }
}

public class MainActivity extends Activity {
    private static final String TAG = "PhiraMod";
    private static final int PICK_FILE = 0x51A1;
    private static final int EXPORT_FILE = 0x51A2;
    /** 弹窗里最多显示多少字符（完整内容可用「复制」拿走）。 */
    private static final int LOG_PREVIEW_CHARS = 4000;

    private QuadSurface view;
    // ---- 刷新率（从 Phira-Pro 移植）----
    /** 允许请求的档位：Rust 侧只允许传这些值，防止传进来一个离谱数字。 */
    private static final int[] SUPPORTED_GAME_REFRESH_RATES = {60, 120, 144, 165, 185, 200, 240};
    private float requestedFrameRate = 120f;
    private float reportedMaxRefreshRate = 0f;
    private boolean activityResumed = false;
    private String displayDiagnostics = "{}";
    private final android.os.Handler refreshHandler = new android.os.Handler(android.os.Looper.getMainLooper());
    /** 窗口 / Surface 变化后系统要一小会儿才稳定：300ms 与 1s 各重试一次，**不**用常驻定时器。 */
    private final Runnable refreshRetry = () -> {
        try {
            applyRequestedFrameRate(false, "retry");
        } catch (Throwable t) {
            CrashLog.stage("[refresh] retry 失败：" + t);
        }
    };
    /** WebView 叠在游戏画面之上（默认隐藏，只有 Rust 请求时才显示）。 */
    private FrameLayout rootLayout;
    private WebViewHost webHost;
    private static String libLoadError = null;
    /** libphira.so 是否已经加载好。加载之前调任何 native 方法都只会抛
     *  UnsatisfiedLinkError —— 而传感器回调每秒几十次，绝不能在库加载前注册：
     *  之前那样做会往日志里灌几百条完整堆栈（每条还要同步写两个文件），
     *  既拖慢启动，又把真正有用的那几行挤出弹窗的预览范围。 */
    private static volatile boolean nativeReady = false;
    /** 游戏主循环是否已经启动（activityOnCreate 返回之后才算）。 */
    private static volatile boolean gameStarted = false;
    /** 同一个 native 方法反复失败时只记第一次，避免把日志刷爆。 */
    private static final java.util.Set<String> reportedFailures =
        java.util.Collections.synchronizedSet(new java.util.HashSet<String>());

    private SensorManager sensorManager;
    private Sensor motionSensor;
    /** 监听器是否在注册状态（unregister 之后还能再注册回来） */
    private boolean motionRegistered = false;
    /** 平滑后的摇动强度（g），避免单帧抖动导致误判 */
    private float motionSmoothed = 0f;
    private final SensorEventListener motionListener = new SensorEventListener() {
        @Override
        public void onSensorChanged(SensorEvent event) {
            float x = event.values[0];
            float y = event.values[1];
            float z = event.values[2];
            float magnitude = (float) Math.sqrt(x * x + y * y + z * z);
            if (event.sensor.getType() == Sensor.TYPE_ACCELEROMETER) {
                // 没有线性加速度传感器时用原始加速度，减掉重力得到近似线性加速度
                magnitude = Math.abs(magnitude - SensorManager.GRAVITY_EARTH);
            }
            motionSmoothed = motionSmoothed * 0.8f + magnitude * 0.2f;
            call("onMotion", () -> QuadNative.onMotion(motionSmoothed / SensorManager.GRAVITY_EARTH));
        }

        @Override
        public void onAccuracyChanged(Sensor sensor, int accuracy) {}
    };

    // ---- 姿态倾斜（给 New UI 的透视视差用） ----
    private Sensor tiltSensor;
    private boolean tiltRegistered = false;
    /** 启动后第一次读到的重力方向，作为「中位」；之后只上报相对它的偏移。 */
    private final float[] tiltNeutral = new float[2];
    private boolean tiltNeutralSet = false;
    private float tiltSmoothX = 0f;
    private float tiltSmoothY = 0f;
    private final SensorEventListener tiltListener = new SensorEventListener() {
        @Override
        public void onSensorChanged(SensorEvent event) {
            float gx = event.values[0];
            float gy = event.values[1];
            if (event.sensor.getType() == Sensor.TYPE_ACCELEROMETER) {
                // 没有重力传感器时，用低通从原始加速度里把重力抠出来
                tiltSmoothX = tiltSmoothX * 0.9f + gx * 0.1f;
                tiltSmoothY = tiltSmoothY * 0.9f + gy * 0.1f;
                gx = tiltSmoothX;
                gy = tiltSmoothY;
            }
            if (!tiltNeutralSet) {
                tiltNeutral[0] = gx;
                tiltNeutral[1] = gy;
                tiltNeutralSet = true;
                return;
            }
            // 相对中位偏移，1g 满量程，×2 让小角度也有明显视差
            final float nx = clampUnit((gx - tiltNeutral[0]) / SensorManager.GRAVITY_EARTH * 2f);
            final float ny = clampUnit(-(gy - tiltNeutral[1]) / SensorManager.GRAVITY_EARTH * 2f);
            call("onTilt", () -> QuadNative.onTilt(nx, ny));
        }

        @Override
        public void onAccuracyChanged(Sensor sensor, int accuracy) {}
    };

    private static float clampUnit(float v) {
        return v < -1f ? -1f : (v > 1f ? 1f : v);
    }

    /**
     * 加载 Rust 的 libphira.so。不要放在 static 块里：静态初始化抛出的异常会变成
     * ExceptionInInitializerError 直接闪退，什么都看不到。
     */
    private static synchronized boolean ensureLibrary() {
        if (libLoadError != null) {
            return false;
        }
        try {
            System.loadLibrary("phira");
            nativeReady = true;
            return true;
        } catch (Throwable t) {
            CrashLog.error("System.loadLibrary(\"phira\") 失败", t);
            libLoadError = t.toString();
            return false;
        }
    }

    /** 调用一个 native 方法，失败就记录下来（不静默吞掉，但同一个方法只记第一次）。 */
    private static void call(String name, Runnable r) {
        try {
            r.run();
        } catch (Throwable t) {
            if (reportedFailures.add(name)) {
                CrashLog.error("native 调用失败: " + name, t);
            }
        }
    }

    /** 离线渲染请求（Rust 侧导出为 Java_org_flos_phira_modded_MainActivity_nativeRenderArg）。 */
    private static native void nativeRenderArg(String arg);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        CrashLog.init(getFilesDir(), getExternalFilesDir(null));
        // 输入框（moe.mivik.inputbox.InputBox）需要一个 Activity 才能弹对话框
        InputBox.setActivity(this);

        Thread.setDefaultUncaughtExceptionHandler((thread, throwable) ->
            CrashLog.error("未捕获异常（Java 侧）", throwable));

        requestWindowFeature(Window.FEATURE_NO_TITLE);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        // 告诉系统：这个界面在前台时，音量键调的是**媒体流**（也就是游戏声音）。
        // 不设这个，系统只能按「最近用过的流」去猜，猜错就变成按音量键在调铃声，
        // 游戏声音纹丝不动。配套的还有 QuadSurface.onKey 里对音量键放行（别吞事件）。
        try {
            setVolumeControlStream(AudioManager.STREAM_MUSIC);
        } catch (Throwable t) {
            Log.w(TAG, "setVolumeControlStream failed", t);
        }

        // 先把上次的日志读走并清掉，再开始写本次的 —— 否则读到的就是本次刚写的行，
        // 每次启动都会弹「上次失败了」。
        final String previous = CrashLog.read(getFilesDir());
        CrashLog.clear(getFilesDir());
        CrashLog.stage("onCreate 进入");

        // 离线渲染触发（手机上没有命令行）：
        //   adb shell am start -n org.flos.phira.modded/.MainActivity -e render "/sdcard/charts/X|X.mp4"
        // 拿不到就不影响正常启动；native 方法缺失也只会记一条日志。
        try {
            String renderArg = getIntent() != null ? getIntent().getStringExtra("render") : null;
            if (renderArg != null && !renderArg.isEmpty()) {
                CrashLog.stage("收到渲染请求: " + renderArg);
                call("nativeRenderArg", () -> nativeRenderArg(renderArg));
            }
        } catch (Throwable t) {
            Log.w(TAG, "读取 render intent 失败", t);
        }
        if (CrashLog.looksLikeFailure(previous)) {
            showPreviousLog(previous);
            return;
        }
        startGame();
    }

    /** 显示上次启动留下的日志（没有 adb 也能看到原因）。 */
    private void showPreviousLog(final String log) {
        CrashLog.stage("检测到上次的日志，显示弹窗");
        final String body = log.length() > LOG_PREVIEW_CHARS
            ? log.substring(log.length() - LOG_PREVIEW_CHARS) + "\n…（已截断，点「复制并继续」拿完整内容）"
            : log;
        new AlertDialog.Builder(this)
            .setTitle("上次启动可能失败了")
            .setMessage(body)
            .setPositiveButton("清除并继续", (d, w) -> startGame())
            .setNeutralButton("复制并继续", (d, w) -> {
                copyToClipboard(log);
                startGame();
            })
            .setNegativeButton("退出", (d, w) -> finish())
            .setCancelable(false)
            .show();
    }

    private void copyToClipboard(String text) {
        try {
            ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (cm != null) {
                cm.setPrimaryClip(ClipData.newPlainText("phira-crash", text));
            }
        } catch (Throwable t) {
            Log.w(TAG, "copyToClipboard failed", t);
        }
    }

    private void startGame() {
        CrashLog.stage("准备加载 libphira.so");
        if (!ensureLibrary()) {
            new AlertDialog.Builder(this)
                .setTitle("Phira Mod")
                .setMessage("无法加载 libphira.so：\n\n" + libLoadError
                    + "\n\n（常见原因：APK 里缺少 libc++_shared.so、或 ABI 不匹配、或 JNI 符号被裁掉）")
                .setPositiveButton("退出", (d, w) -> finish())
                .setCancelable(false)
                .show();
            return;
        }
        CrashLog.stage("libphira.so 加载成功");
        // 库好了才注册传感器（onResume 那次会因为 nativeReady=false 直接跳过）
        startMotionSensor();

        final MainActivity self = this;

        // 顺序要紧：initializeContext 负责把 Activity 绑到 ndk_context（读 APK 内 assets、
        // 取目录都依赖它），Rust 侧 initialize_raw 又是 .unwrap()，所以它必须最先。
        call("initializeContext", () -> QuadNative.initializeContext(self));
        CrashLog.stage("initializeContext 完成（ndk_context + rustls）");
        call("setDataPath", () -> QuadNative.setDataPath(getFilesDir().getAbsolutePath()));
        call("setTempDir", () -> QuadNative.setTempDir(getCacheDir().getAbsolutePath()));
        call("setDpi", () -> {
            DisplayMetrics dm = getResources().getDisplayMetrics();
            QuadNative.setDpi(dm.densityDpi);
        });
        CrashLog.stage("数据目录 / 临时目录 / DPI 已交给 Rust");
        call("initializeEnvironment", () -> QuadNative.initializeEnvironment());
        CrashLog.stage("initializeEnvironment 完成（inputbox 后端）");

        view = new QuadSurface(this);
        // 用 FrameLayout 包一层：游戏画面在底层，WebView 叠在上面（默认 GONE）。
        // 这样以后 WebView 界面层可以在安卓上复用同一套 HTML，不影响现有原生渲染。
        rootLayout = new FrameLayout(this);
        rootLayout.addView(view, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        setContentView(rootLayout);
        CrashLog.stage("SurfaceView 就绪（FrameLayout 已就位）");

        // 懒创建 WebView：失败也绝不能拖垮游戏启动
        try {
            webHost = new WebViewHost();
            webHost.init(this, rootLayout);
            CrashLog.stage("WebView 宿主已创建（隐藏）");
        } catch (Throwable t) {
            webHost = null;
            Log.w(TAG, "WebView 宿主创建失败（不影响原生界面）", t);
            CrashLog.stage("WebView 宿主创建失败：" + t);
        }

        // miniquad 的 activityOnCreate 会启动游戏（内部起线程后返回，游戏在别的线程跑）
        CrashLog.stage("调用 activityOnCreate");
        call("activityOnCreate", () -> QuadNative.activityOnCreate(self));
        // 走到这里说明 activityOnCreate 已经返回了。正常情况游戏已经在自己的线程上跑；
        // 如果宿主版本没有在 activityOnCreate 里启动，就兜底再来一次（Rust 侧幂等）。
        CrashLog.stage("activityOnCreate 已返回，startApp 兜底（Rust 侧幂等）");
        call("startApp", () -> QuadNative.startApp());
        gameStarted = true;
        CrashLog.stage("startApp 已返回（游戏线程已在运行，或已经结束）");
    }

    @Override
    public boolean dispatchKeyEvent(android.view.KeyEvent event) {
        // 原生返回键：WebView 可见且能历史后退时（第九章地图 → 曲库等），让 WebView
        // 自己后退并消费事件；否则走游戏原有路径（暂停菜单等）。
        // ⚠️ 用 dispatchKeyEvent 而不是 onBackPressed：SurfaceView 持焦点时按键
        // 可能不走到 onBackPressed，这里在 Activity 层最早拦截。
        if (event.getAction() == android.view.KeyEvent.ACTION_DOWN
                && event.getKeyCode() == android.view.KeyEvent.KEYCODE_BACK
                && webHost != null && webHost.goBack()) {
            return true;
        }
        return super.dispatchKeyEvent(event);
    }

    @Override
    protected void onResume() {
        super.onResume();
        InputBox.setActivity(this);
        // 库还没加载完就别调 native：onResume 通常早于 startGame（弹「上次日志」时更是
        // 早得多），那时调用只会抛 UnsatisfiedLinkError —— 记进日志的 [error] 会让
        // 下次启动误报「上次启动失败了」，还会把真正的错误淹掉。
        if (nativeReady) {
            call("activityOnResume", () -> QuadNative.activityOnResume());
            call("prprActivityOnResume", () -> QuadNative.prprActivityOnResume());
        }
        activityResumed = true;
        requestRefreshRateUpdate("resume");
        startMotionSensor();
    }

    /** 注册加速度计：摇一摇再玩要靠它（引擎在设置里开关）。 */
    private void startMotionSensor() {
        try {
            if (!nativeReady || motionRegistered) {
                // 库还没加载完（onResume 常常早于 startGame）或已经注册过了
                return;
            }
            if (sensorManager == null) {
                sensorManager = (SensorManager) getSystemService(Context.SENSOR_SERVICE);
            }
            if (sensorManager == null) {
                return;
            }
            if (motionSensor == null) {
                // 优先用线性加速度（系统已经去掉重力），没有就退回原始加速度计
                motionSensor = sensorManager.getDefaultSensor(Sensor.TYPE_LINEAR_ACCELERATION);
                if (motionSensor == null) {
                    motionSensor = sensorManager.getDefaultSensor(Sensor.TYPE_ACCELEROMETER);
                }
            }
            if (motionSensor == null) {
                CrashLog.stage("没有可用的加速度计，摇一摇不可用");
                return;
            }
            sensorManager.registerListener(motionListener, motionSensor, SensorManager.SENSOR_DELAY_GAME);
            motionRegistered = true;
            CrashLog.stage("加速度计已注册: " + motionSensor.getName());
            startTiltSensor();
        } catch (Throwable t) {
            CrashLog.error("startMotionSensor failed", t);
        }
    }

    /** 注册姿态传感器（重力优先，退回加速度计）。没有就静默跳过，视差会回退到鼠标。 */
    private void startTiltSensor() {
        try {
            if (sensorManager == null || tiltRegistered) {
                return;
            }
            if (tiltSensor == null) {
                tiltSensor = sensorManager.getDefaultSensor(Sensor.TYPE_GRAVITY);
                if (tiltSensor == null) {
                    tiltSensor = sensorManager.getDefaultSensor(Sensor.TYPE_ACCELEROMETER);
                }
            }
            if (tiltSensor == null) {
                CrashLog.stage("没有可用的姿态传感器，透视视差回退到鼠标");
                return;
            }
            sensorManager.registerListener(tiltListener, tiltSensor, SensorManager.SENSOR_DELAY_GAME);
            tiltRegistered = true;
            CrashLog.stage("姿态传感器已注册: " + tiltSensor.getName());
        } catch (Throwable t) {
            CrashLog.error("startTiltSensor failed", t);
        }
    }

    private void stopMotionSensor() {
        try {
            if (sensorManager != null && tiltSensor != null && tiltRegistered) {
                sensorManager.unregisterListener(tiltListener, tiltSensor);
                tiltRegistered = false;
            }
            if (sensorManager != null && motionSensor != null && motionRegistered) {
                sensorManager.unregisterListener(motionListener, motionSensor);
                motionRegistered = false;
            }
        } catch (Throwable ignored) {
        }
    }

    /** Rust 侧通过 JNI 调这个（对应 `set_refresh_rate`）。 */
    public void setRequestedFrameRate(int frameRate) {
        boolean supported = false;
        for (int rate : SUPPORTED_GAME_REFRESH_RATES) {
            supported |= frameRate == rate;
        }
        if (!supported) {
            CrashLog.stage("[refresh] 拒绝不支持的刷新率：" + frameRate);
            return;
        }
        runOnUiThread(() -> {
            requestedFrameRate = frameRate;
            requestRefreshRateUpdate("user setting");
        });
    }

    void requestRefreshRateUpdate(String reason) {
        try {
            if (view == null) {
                return;
            }
            refreshHandler.removeCallbacks(refreshRetry);
            applyRequestedFrameRate(true, reason);
            // 窗口/Surface 稳定后再补两次，避免首次申请落在过渡态上
            refreshHandler.postDelayed(refreshRetry, 300);
            refreshHandler.postDelayed(refreshRetry, 1000);
        } catch (Throwable t) {
            CrashLog.stage("[refresh] 申请失败：" + t);
        }
    }

    @Override
    public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (activityResumed) {
            requestRefreshRateUpdate(focused ? "focus restored" : "visible under overlay");
        }
    }

    @Override
    public void onTopResumedActivityChanged(boolean topResumed) {
        super.onTopResumedActivityChanged(topResumed);
        if (activityResumed) {
            requestRefreshRateUpdate(topResumed ? "top resumed" : "multi-resume under overlay");
        }
    }

    @Override
    public void onConfigurationChanged(android.content.res.Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        requestRefreshRateUpdate("configuration changed");
    }

    @Override
    public void onMultiWindowModeChanged(boolean isInMultiWindowMode) {
        super.onMultiWindowModeChanged(isInMultiWindowMode);
        requestRefreshRateUpdate(isInMultiWindowMode ? "multi-window" : "fullscreen");
    }

    public String getDisplayDiagnostics() {
        return displayDiagnostics;
    }

    private void applyRequestedFrameRate(boolean force, String reason) {
        Display display = getWindowManager().getDefaultDisplay();
        float maxRefreshRate = display.getRefreshRate();
        float selectedRate = -1f;
        Display.Mode selectedMode = null;
        int selectedModeId = 0;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            Display.Mode currentMode = display.getMode();
            Display.Mode[] modes = display.getSupportedModes();
            int[] widths = new int[modes.length];
            int[] heights = new int[modes.length];
            float[] rates = new float[modes.length];
            for (int i = 0; i < modes.length; i++) {
                Display.Mode mode = modes[i];
                float rate = mode.getRefreshRate();
                widths[i] = mode.getPhysicalWidth();
                heights[i] = mode.getPhysicalHeight();
                rates[i] = rate;
                maxRefreshRate = Math.max(maxRefreshRate, rate);
                // S+ 上系统可能只报了 modes 没报 alternative，得把它也算进「设备上限」
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    for (float alternative : mode.getAlternativeRefreshRates()) {
                        maxRefreshRate = Math.max(maxRefreshRate, alternative);
                    }
                }
            }
            int selected = RefreshRatePolicy.legacyMode(requestedFrameRate, currentMode.getPhysicalWidth(),
                    currentMode.getPhysicalHeight(), widths, heights, rates);
            selectedMode = selected >= 0 ? modes[selected] : currentMode;
            selectedRate = selectedMode.getRefreshRate();
            selectedModeId = selectedMode.getModeId();
        } else {
            for (float rate : display.getSupportedRefreshRates()) {
                maxRefreshRate = Math.max(maxRefreshRate, rate);
                if (rate <= requestedFrameRate + 0.5f && rate > selectedRate) {
                    selectedRate = rate;
                }
            }
            if (selectedRate < 0f) {
                for (float rate : display.getSupportedRefreshRates()) {
                    if (selectedRate < 0f || rate < selectedRate) {
                        selectedRate = rate;
                    }
                }
            }
            if (selectedRate < 0f) {
                selectedRate = display.getRefreshRate();
            }
        }
        // 只在变化时上报，避免每秒几十次 JNI 往返
        if (Math.abs(reportedMaxRefreshRate - maxRefreshRate) > 0.01f && nativeReady) {
            reportedMaxRefreshRate = maxRefreshRate;
            try {
                QuadNative.setMaxRefreshRate(maxRefreshRate);
            } catch (Throwable t) {
                CrashLog.stage("[refresh] setMaxRefreshRate 失败：" + t);
            }
        }
        Window window = getWindow();
        WindowManager.LayoutParams attributes = window.getAttributes();
        float windowRate = RefreshRatePolicy.windowRate(Build.VERSION.SDK_INT, requestedFrameRate, selectedRate);
        int windowMode = RefreshRatePolicy.windowModeId(Build.VERSION.SDK_INT, selectedModeId);
        boolean changed = Math.abs(attributes.preferredRefreshRate - windowRate) > 0.01f;
        attributes.preferredRefreshRate = windowRate;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            changed |= attributes.preferredDisplayModeId != windowMode;
            attributes.preferredDisplayModeId = windowMode;
        }
        if (changed || force) {
            window.setAttributes(attributes);
        }
        // Android 11+ 保留用户的投票：临时的 60Hz 模式上报不能悄悄顶掉 144Hz 的请求
        if (view != null) {
            view.setTargetFrameRate(Build.VERSION.SDK_INT >= Build.VERSION_CODES.R ? requestedFrameRate : selectedRate,
                    force, force && view.hasWindowFocus());
        }
        recordDisplayState(display, windowRate, windowMode, maxRefreshRate, reason);
    }

    /** 显示状态诊断（供「设置 → 日志」页看，排查分屏/浮窗下帧率不生效）。 */
    private void recordDisplayState(Display display, float windowRate, int windowMode, float maximum, String reason) {
        try {
            android.os.PowerManager power = (android.os.PowerManager) getSystemService(POWER_SERVICE);
            boolean multiWindow = Build.VERSION.SDK_INT >= Build.VERSION_CODES.N && isInMultiWindowMode();
            org.json.JSONObject state = new org.json.JSONObject()
                    .put("reason", reason)
                    .put("requested_hz", requestedFrameRate)
                    .put("reported_display_hz", display.getRefreshRate())
                    .put("reported_maximum_hz", maximum)
                    .put("window_preferred_hz", windowRate)
                    .put("window_mode_id", windowMode)
                    .put("multi_window", multiWindow)
                    .put("low_power", power != null && power.isPowerSaveMode())
                    .put("surface_valid", view != null && view.getHolder().getSurface().isValid());
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && power != null) {
                state.put("thermal_status", power.getCurrentThermalStatus());
            }
            displayDiagnostics = state.toString();
        } catch (Throwable t) {
            CrashLog.stage("[refresh] 诊断失败：" + t);
        }
    }

    protected void onPause() {
        super.onPause();
        activityResumed = false;
        // 后台了就别再重试（避免空转），恢复时 onResume 会重新申请
        refreshHandler.removeCallbacks(refreshRetry);
        stopMotionSensor();
        // 游戏没启动过就没什么可暂停的（miniquad 的消息通道还不存在）
        if (gameStarted) {
            call("prprActivityOnPause", () -> QuadNative.prprActivityOnPause());
            call("activityOnPause", () -> QuadNative.activityOnPause());
        }
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        refreshHandler.removeCallbacks(refreshRetry);
        InputBox.setActivity(null);
        CrashLog.stage("[exit] onDestroy（正常退出）");
        if (gameStarted) {
            call("prprActivityOnDestroy", () -> QuadNative.prprActivityOnDestroy());
            call("activityOnDestroy", () -> QuadNative.activityOnDestroy());
        }
        if (nativeReady) {
            call("releaseContext", () -> QuadNative.releaseContext());
        }
    }

    // ------------------------------------------------------------------
    // 以下方法是 Rust 侧按名字回调的（GetMethodID），缺任何一个都可能直接 abort
    // ------------------------------------------------------------------

    /** miniquad: set_fullscreen() */
    @SuppressWarnings("deprecation")
    public void setFullScreen(final boolean fullscreen) {
        runOnUiThread(() -> {
            try {
                Window window = getWindow();
                View decor = window.getDecorView();
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    window.setDecorFitsSystemWindows(!fullscreen);
                }
                if (fullscreen) {
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                        WindowManager.LayoutParams lp = window.getAttributes();
                        lp.layoutInDisplayCutoutMode =
                            WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
                        window.setAttributes(lp);
                    }
                    decor.setSystemUiVisibility(
                        View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                            | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                            | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                            | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                            | View.SYSTEM_UI_FLAG_FULLSCREEN
                            | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY);
                } else {
                    decor.setSystemUiVisibility(View.SYSTEM_UI_FLAG_VISIBLE);
                }
            } catch (Throwable t) {
                CrashLog.error("setFullScreen failed", t);
            }
        });
    }

    /** miniquad: show_keyboard() */
    public void showKeyboard(final boolean show) {
        runOnUiThread(() -> {
            try {
                InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
                if (imm == null) {
                    return;
                }
                if (show) {
                    if (view == null) {
                        CrashLog.stage("showKeyboard: view == null");
                        return;
                    }
                    view.setFocusableInTouchMode(true);
                    view.requestFocus();
                    boolean ok = imm.showSoftInput(view, 0);
                    // ⚠️ 原来这里**完全不看返回值** —— showSoftInput 失败是静默的，
                    // 表现就是「连键盘都召唤不出来」，而且事后无从查起。现在记进崩溃日志
                    // （设置 → 日志页能看到），失败时再走强制弹出兜底。
                    CrashLog.stage("showKeyboard(true) ok=" + ok
                            + " focus=" + view.hasFocus()
                            + " winFocus=" + view.hasWindowFocus()
                            + " token=" + (view.getWindowToken() != null));
                    if (!ok) {
                        imm.toggleSoftInput(InputMethodManager.SHOW_FORCED, 0);
                        CrashLog.stage("showKeyboard(true): fallback toggleSoftInput(SHOW_FORCED)");
                    }
                } else if (view != null) {
                    imm.hideSoftInputFromWindow(view.getWindowToken(), 0);
                }
            } catch (Throwable t) {
                CrashLog.error("showKeyboard failed", t);
            }
        });
    }

    /** miniquad: clipboard_set()，在 context（也就是这个 Activity）上按名字调用 */
    public void copy(final String data) {
        copyToClipboard(data);
    }

    /** prpr::ext::open_url()，点应用内的链接（账号设置、公告、谱面作者链接等） */
    public void openUrl(final String url) {
        runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
                intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                startActivity(intent);
            } catch (Throwable t) {
                // 没有浏览器 / 无法处理这个 scheme 时不要崩
                CrashLog.error("openUrl failed: " + url, t);
            }
        });
    }

    /** phira::page::library::request_export()，导出谱面时弹出「保存到哪」 */
    public void showExportDialog(final String suggestedName) {
        runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType("application/zip");
                intent.putExtra(Intent.EXTRA_TITLE, suggestedName);
                startActivityForResult(intent, EXPORT_FILE);
            } catch (Throwable t) {
                CrashLog.error("showExportDialog failed", t);
            }
        });
    }

    /** phira::page::library::delete_uri()，导出流程结束后清掉 SAF 文档 */
    public void deleteUri(final Uri uri) {
        try {
            if (uri != null) {
                getContentResolver().delete(uri, null, null);
            }
        } catch (Throwable t) {
            CrashLog.error("deleteUri failed", t);
        }
    }

    /** 由 Rust 通过 JNI 调用（prpr::scene::request_file）。 */
    public void chooseFile() {
        runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType("*/*");
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                startActivityForResult(intent, PICK_FILE);
            } catch (Throwable t) {
                CrashLog.error("chooseFile failed", t);
                call("setChosenFile(empty)", () -> QuadNative.setChosenFile(""));
            }
        });
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == EXPORT_FILE) {
            if (resultCode != RESULT_OK || data == null || data.getData() == null) {
                CrashLog.stage("导出被取消");
                return;
            }
            final Uri uri = data.getData();
            try {
                ParcelFileDescriptor pfd = getContentResolver().openFileDescriptor(uri, "w");
                if (pfd == null) {
                    CrashLog.error("导出：openFileDescriptor 返回 null", null);
                    return;
                }
                // detachFd 把 fd 的所有权交出去，Rust 侧会用 from_raw_fd 接管
                final int fd = pfd.detachFd();
                call("processExportFd", () -> QuadNative.processExportFd(uri, fd));
                CrashLog.stage("导出：fd 已交给 Rust");
            } catch (Throwable t) {
                CrashLog.error("导出失败", t);
            }
            return;
        }
        if (requestCode != PICK_FILE) {
            return;
        }
        if (resultCode != RESULT_OK || data == null || data.getData() == null) {
            call("setChosenFile(empty)", () -> QuadNative.setChosenFile(""));
            return;
        }
        final Uri uri = data.getData();
        new Thread(() -> {
            File dst = new File(getCacheDir(), "chosen_" + System.currentTimeMillis());
            try (InputStream in = getContentResolver().openInputStream(uri); FileOutputStream out = new FileOutputStream(dst)) {
                byte[] buf = new byte[1 << 16];
                int n;
                while ((n = in.read(buf)) > 0) {
                    out.write(buf, 0, n);
                }
                final String path = dst.getAbsolutePath();
                call("setChosenFile", () -> QuadNative.setChosenFile(path));
            } catch (Throwable t) {
                CrashLog.error("复制所选文件失败", t);
                call("setChosenFile(empty)", () -> QuadNative.setChosenFile(""));
            }
        }).start();
    }

    /**
     * singleTask 启动模式下，第三方文件管理器可能把结果投递到这里而不是 onActivityResult。
     * 目前 manifest 没有注册 VIEW/SEND 的 intent-filter，所以这里只更新 intent，不做导入。
     */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
    }
}
