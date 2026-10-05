// iOS 加速度计包装（CoreMotion），给「摇一摇再玩」用。
//
// 只被 phira/build.rs 在 iOS 目标下编译，其它平台不参与构建。
// 对外只暴露三个 C 函数，Rust 侧在 phira/src/lib.rs 里声明同名 extern。

#import <CoreMotion/CoreMotion.h>
#import <math.h>

static CMMotionManager *gManager = nil;

/// 启动采集；返回 1 表示传感器可用，0 表示这台设备没有加速度计（模拟器等）。
int phira_motion_start(void) {
    if (gManager == nil) {
        gManager = [[CMMotionManager alloc] init];
    }
    if (!gManager.isAccelerometerAvailable) {
        return 0;
    }
    gManager.accelerometerUpdateInterval = 1.0 / 60.0;
    [gManager startAccelerometerUpdates];
    return 1;
}

void phira_motion_stop(void) {
    if (gManager == nil) {
        return;
    }
    [gManager stopAccelerometerUpdates];
    [gManager release];
    gManager = nil;
}

/// 当前摇动强度（单位 g，已去掉重力）；返回负数表示传感器不可用。
float phira_motion_level(void) {
    if (gManager == nil || !gManager.isAccelerometerAvailable) {
        return -1.0f;
    }
    CMAccelerometerData *data = gManager.accelerometerData;
    if (data == nil) {
        return 0.0f;
    }
    CMAcceleration a = data.acceleration;
    // CoreMotion 的 acceleration 含重力（静止时模长约 1g），这里减掉当作线性加速度，
    // 和 Android 侧退回原始加速度计时的处理保持一致。
    double magnitude = sqrt(a.x * a.x + a.y * a.y + a.z * a.z) - 1.0;
    if (magnitude < 0.0) {
        magnitude = -magnitude;
    }
    return (float) magnitude;
}


// ---- 姿态倾斜（给 New UI 的透视视差用） ----
//
// 和 Android 侧同一套语义：只报**相对启动时中位的偏移**，归一化到 [-1, 1]。
// 直接报原始重力方向是不行的 —— 横屏握持这个静态姿势本身就会把偏移顶到 ±1。

static float gTiltNeutralX = NAN;
static float gTiltNeutralY = NAN;
static float gTiltSmoothX = 0.f;
static float gTiltSmoothY = 0.f;

/// 取当前姿态倾斜。返回 1 表示可用，0 表示没有传感器（此时不写 out 参数）。
int phira_motion_tilt(float *out_x, float *out_y) {
    if (gManager == nil || !gManager.isAccelerometerAvailable) {
        return 0;
    }
    CMAccelerometerData *data = gManager.accelerometerData;
    if (data == nil) {
        return 0;
    }
    CMAcceleration a = data.acceleration;
    // 低通把重力抠出来（原始加速度计含高频抖动）
    gTiltSmoothX = gTiltSmoothX * 0.9f + (float) a.x * 0.1f;
    gTiltSmoothY = gTiltSmoothY * 0.9f + (float) a.y * 0.1f;

    if (isnan(gTiltNeutralX)) {
        gTiltNeutralX = gTiltSmoothX;
        gTiltNeutralY = gTiltSmoothY;
        if (out_x) *out_x = 0.f;
        if (out_y) *out_y = 0.f;
        return 1;
    }

    // 1g 满量程，×2 让小角度也有明显视差
    float nx = (gTiltSmoothX - gTiltNeutralX) * 2.f;
    float ny = -(gTiltSmoothY - gTiltNeutralY) * 2.f;
    if (out_x) *out_x = nx < -1.f ? -1.f : (nx > 1.f ? 1.f : nx);
    if (out_y) *out_y = ny < -1.f ? -1.f : (ny > 1.f ? 1.f : ny);
    return 1;
}
