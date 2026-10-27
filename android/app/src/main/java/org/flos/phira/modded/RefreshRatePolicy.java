package org.flos.phira.modded;

/**
 * 纯选择规则，Activity 和宿主 JVM 回归测试共用（无 Android 依赖）。
 *
 * 从 Phira-Pro 的 RefreshRatePolicy 移植：
 * - legacyMode：在「当前分辨率」的模式里挑 ≤ 请求值 + 0.5 的最大刷新率；
 *   没有就退而求其次取最低的那个（避免系统给的极端模式）。
 * - windowModeId：Android 11+ 只用 Surface 投票，不要钉死物理模式——
 *   否则会盖过 preferredRefreshRate，还会干扰分屏缩放。
 * - windowRate：Android 14+ 直接用请求值；11..13 不留 60Hz 的窗口投票
 *   （Surface 请求 144Hz 时，系统过滤过的 supportedModes 会和它打架）；10 以下用旧值。
 */
final class RefreshRatePolicy {
    private static final float EPSILON_HZ = 0.5f;

    static int legacyMode(float requested, int width, int height, int[] widths, int[] heights, float[] rates) {
        int selected = -1;
        int lowest = -1;
        for (int i = 0; i < rates.length; i++) {
            if (widths[i] != width || heights[i] != height || Float.isNaN(rates[i]) || Float.isInfinite(rates[i]) || rates[i] <= 0f) {
                continue;
            }
            if (lowest < 0 || rates[i] < rates[lowest]) {
                lowest = i;
            }
            if (rates[i] <= requested + EPSILON_HZ && (selected < 0 || rates[i] > rates[selected])) {
                selected = i;
            }
        }
        return selected >= 0 ? selected : lowest;
    }

    static int windowModeId(int sdk, int legacyId) {
        return sdk >= 30 ? 0 : legacyId;
    }

    static float windowRate(int sdk, float requested, float legacyRate) {
        if (sdk >= 34) {
            return requested;
        }
        return sdk >= 30 ? 0f : legacyRate;
    }

    private RefreshRatePolicy() {}
}
