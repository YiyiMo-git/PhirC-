// 离线渲染的 iOS 视频编码器：AVAssetWriter（VideoToolbox 硬件 H.264）。
//
// 与 ime.m / motion.m 同一套路：只在 iOS 目标下编译（见 build.rs 的 build_ios_video），
// **MRC（不启用 ARC）**，静态对象手动 release。
//
// 接口（对应 phira/src/record.rs 的 ios_sink）：
//   phira_video_start(name, w, h, fps, bitrate_kbps) -> ctx
//   phira_video_append(ctx, rgba, pts_us) -> bool
//   phira_video_finish(ctx) -> bool
//   phira_video_cancel(ctx)
//   phira_video_last_path() -> const char*（写完后可读，指向静态缓冲）
//
// 帧数据是 RGBA（和 GL 回读一致），这里逐像素换成 BGRA 写进 CVPixelBuffer
// （kCVPixelFormatType_32BGRA 是 AVAssetWriter 最稳的像素格式）。

#import <AVFoundation/AVFoundation.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>

typedef struct {
    AVAssetWriter *writer;
    AVAssetWriterInput *input;
    AVAssetWriterInputPixelBufferAdaptor *adaptor;
    int width;
    int height;
    int fps;
    char path[1024];
} PhiraVideoCtx;

static char g_last_path[1024];

const char *phira_video_last_path(void) { return g_last_path; }

void *phira_video_start(const char *name, int w, int h, int fps, int bitrate_kbps) {
    @autoreleasepool {
        if (w <= 0 || h <= 0 || fps <= 0) {
            return NULL;
        }
        NSString *docs = [NSSearchPathForDirectoriesInDomains(NSDocumentDirectory, NSUserDomainMask, YES) firstObject];
        if (docs == nil) {
            return NULL;
        }
        NSString *fileName = name != NULL ? [NSString stringWithUTF8String:name] : @"render.mp4";
        NSString *path = [docs stringByAppendingPathComponent:fileName];
        [[NSFileManager defaultManager] removeItemAtPath:path error:nil];
        NSURL *url = [NSURL fileURLWithPath:path];

        NSError *err = nil;
        AVAssetWriter *writer = [[AVAssetWriter alloc] initWithURL:url fileType:AVFileTypeMPEG4 error:&err];
        if (writer == nil) {
            NSLog(@"[phira-video] AVAssetWriter 创建失败: %@", err);
            return NULL;
        }
        NSDictionary *compression = @{
            AVVideoAverageBitRateKey : @(bitrate_kbps * 1000),
            AVVideoMaxKeyFrameIntervalKey : @(fps),
            AVVideoProfileLevelKey : AVVideoProfileLevelH264HighAutoLevel,
            AVVideoAllowFrameReorderingKey : @NO,
        };
        NSDictionary *settings = @{
            AVVideoCodecKey : AVVideoCodecTypeH264,
            AVVideoWidthKey : @(w),
            AVVideoHeightKey : @(h),
            AVVideoCompressionPropertiesKey : compression,
        };
        AVAssetWriterInput *input = [[AVAssetWriterInput alloc] initWithMediaType:AVMediaTypeVideo outputSettings:settings];
        input.expectsMediaDataInRealTime = NO;

        NSDictionary *attrs = @{
            (id)kCVPixelBufferPixelFormatTypeKey : @(kCVPixelFormatType_32BGRA),
            (id)kCVPixelBufferWidthKey : @(w),
            (id)kCVPixelBufferHeightKey : @(h),
            (id)kCVPixelBufferIOSurfacePropertiesKey : @{},
        };
        AVAssetWriterInputPixelBufferAdaptor *adaptor =
            [[AVAssetWriterInputPixelBufferAdaptor alloc] initWithAssetWriterInput:input sourcePixelBufferAttributes:attrs];

        if (![writer canAddInput:input]) {
            NSLog(@"[phira-video] writer 不接受该 input");
            [adaptor release];
            [input release];
            [writer release];
            return NULL;
        }
        [writer addInput:input];
        if (![writer startWriting]) {
            NSLog(@"[phira-video] startWriting 失败: %@", writer.error);
            [adaptor release];
            [input release];
            [writer release];
            return NULL;
        }
        [writer startSessionAtSourceTime:kCMTimeZero];

        PhiraVideoCtx *ctx = (PhiraVideoCtx *)calloc(1, sizeof(PhiraVideoCtx));
        ctx->writer = writer;
        ctx->input = input;
        ctx->adaptor = adaptor;
        ctx->width = w;
        ctx->height = h;
        ctx->fps = fps;
        snprintf(ctx->path, sizeof(ctx->path), "%s", path.UTF8String);
        snprintf(g_last_path, sizeof(g_last_path), "%s", path.UTF8String);
        NSLog(@"[phira-video] 开始写入 %@（%dx%d@%d, %d kbps）", path, w, h, fps, bitrate_kbps);
        return ctx;
    }
}

bool phira_video_append(void *opaque, const uint8_t *rgba, int64_t pts_us) {
    PhiraVideoCtx *ctx = (PhiraVideoCtx *)opaque;
    if (ctx == NULL || rgba == NULL) {
        return false;
    }
    @autoreleasepool {
        CVPixelBufferRef pb = NULL;
        NSDictionary *attrs = @{
            (id)kCVPixelBufferPixelFormatTypeKey : @(kCVPixelFormatType_32BGRA),
            (id)kCVPixelBufferWidthKey : @(ctx->width),
            (id)kCVPixelBufferHeightKey : @(ctx->height),
        };
        CVReturn r = CVPixelBufferCreate(kCFAllocatorDefault, ctx->width, ctx->height, kCVPixelFormatType_32BGRA,
                                         (__bridge CFDictionaryRef)attrs, &pb);
        if (r != kCVReturnSuccess || pb == NULL) {
            return false;
        }
        CVPixelBufferLockBaseAddress(pb, 0);
        uint8_t *dst = (uint8_t *)CVPixelBufferGetBaseAddress(pb);
        size_t stride = CVPixelBufferGetBytesPerRow(pb);
        const size_t row_bytes = (size_t)ctx->width * 4;
        for (int y = 0; y < ctx->height; y++) {
            const uint8_t *src = rgba + (size_t)y * row_bytes;
            uint8_t *d = dst + (size_t)y * stride;
            for (int x = 0; x < ctx->width; x++) {
                d[x * 4 + 0] = src[x * 4 + 2]; // B
                d[x * 4 + 1] = src[x * 4 + 1]; // G
                d[x * 4 + 2] = src[x * 4 + 0]; // R
                d[x * 4 + 3] = 255;
            }
        }
        CVPixelBufferUnlockBaseAddress(pb, 0);

        // 编码器忙时等一会儿（离线渲染，宁可等也不要丢帧）
        int spins = 0;
        while (!ctx->input.readyForMoreMediaData && spins < 2000) {
            [NSThread sleepForTimeInterval:0.002];
            spins++;
        }
        BOOL ok = [ctx->adaptor appendPixelBuffer:pb withPresentationTime:CMTimeMake(pts_us, 1000000)];
        CVPixelBufferRelease(pb);
        if (!ok) {
            NSLog(@"[phira-video] append 失败: %@", ctx->writer.error);
        }
        return ok ? true : false;
    }
}

bool phira_video_finish(void *opaque) {
    PhiraVideoCtx *ctx = (PhiraVideoCtx *)opaque;
    if (ctx == NULL) {
        return false;
    }
    @autoreleasepool {
        [ctx->input markAsFinished];
        __block BOOL done = NO;
        [ctx->writer finishWritingWithCompletionHandler:^{
            done = YES;
        }];
        int spins = 0;
        while (!done && spins < 60000) { // 最多等 60s
            [NSThread sleepForTimeInterval:0.001];
            spins++;
        }
        BOOL ok = (ctx->writer.status == AVAssetWriterStatusCompleted);
        if (!ok) {
            NSLog(@"[phira-video] finish 状态异常: %ld err=%@", (long)ctx->writer.status, ctx->writer.error);
        }
        [ctx->adaptor release];
        [ctx->input release];
        [ctx->writer release];
        free(ctx);
        return ok ? true : false;
    }
}

void phira_video_cancel(void *opaque) {
    PhiraVideoCtx *ctx = (PhiraVideoCtx *)opaque;
    if (ctx == NULL) {
        return;
    }
    @autoreleasepool {
        [ctx->writer cancelWriting];
        [ctx->adaptor release];
        [ctx->input release];
        [ctx->writer release];
        free(ctx);
    }
}

// ============================================================
// 混音：把音乐（原文件直通，不重编码视频）并进已经写好的 mp4。
//
// 做法：AVMutableComposition 里放「视频轨（原样）+ 音乐轨（延迟 delay_us）」，
// 再用 AVAssetExportPresetPassthrough 导出 —— 视频流不重新编码，画质无损；
// 音频若是 MP4 容器不接受的编码（例如 ogg），导出会失败，调用方保留无声视频。
// ============================================================
bool phira_video_mux_audio(const char *video_path, const char *music_path, long long delay_us) {
    if (video_path == NULL || music_path == NULL) {
        return false;
    }
    @autoreleasepool {
        NSFileManager *fm = [NSFileManager defaultManager];
        NSURL *videoURL = [NSURL fileURLWithPath:[NSString stringWithUTF8String:video_path]];
        NSURL *musicURL = [NSURL fileURLWithPath:[NSString stringWithUTF8String:music_path]];
        if (![fm fileExistsAtPath:videoURL.path] || ![fm fileExistsAtPath:musicURL.path]) {
            NSLog(@"[phira-video] mux: 文件不存在");
            return false;
        }
        AVURLAsset *videoAsset = [AVURLAsset assetWithURL:videoURL];
        AVURLAsset *musicAsset = [AVURLAsset assetWithURL:musicURL];
        AVAssetTrack *videoTrack = [[videoAsset tracksWithMediaType:AVMediaTypeVideo] firstObject];
        AVAssetTrack *audioTrack = [[musicAsset tracksWithMediaType:AVMediaTypeAudio] firstObject];
        if (videoTrack == nil || audioTrack == nil) {
            NSLog(@"[phira-video] mux: 缺少视频轨或音频轨");
            return false;
        }

        AVMutableComposition *comp = [AVMutableComposition composition];
        NSError *err = nil;
        AVMutableCompositionTrack *vc = [comp addMutableTrackWithMediaType:AVMediaTypeVideo
                                                          preferredTrackID:kCMPersistentTrackID_Invalid];
        if (![vc insertTimeRange:CMTimeRangeMake(kCMTimeZero, videoAsset.duration)
                          ofTrack:videoTrack
                           atTime:kCMTimeZero
                            error:&err]) {
            NSLog(@"[phira-video] mux: 插入视频轨失败 %@", err);
            return false;
        }
        AVMutableCompositionTrack *ac = [comp addMutableTrackWithMediaType:AVMediaTypeAudio
                                                          preferredTrackID:kCMPersistentTrackID_Invalid];
        CMTime delay = CMTimeMake(delay_us > 0 ? delay_us : 0, 1000000);
        if (![ac insertTimeRange:CMTimeRangeMake(kCMTimeZero, musicAsset.duration)
                          ofTrack:audioTrack
                           atTime:delay
                            error:&err]) {
            NSLog(@"[phira-video] mux: 插入音轨失败 %@", err);
            return false;
        }

        NSURL *tmpURL = [[videoURL URLByDeletingPathExtension] URLByAppendingPathExtension:@"mux.mp4"];
        [fm removeItemAtURL:tmpURL error:nil];
        AVAssetExportSession *ex = [[AVAssetExportSession alloc] initWithAsset:comp
                                                                  presetName:AVAssetExportPresetPassthrough];
        if (ex == nil) {
            NSLog(@"[phira-video] mux: 导出器创建失败");
            return false;
        }
        ex.outputURL = tmpURL;
        ex.outputFileType = AVFileTypeMPEG4;
        // 只导出视频长度，避免音乐比谱面长时尾巴拖出一段黑屏
        ex.timeRange = CMTimeRangeMake(kCMTimeZero, videoAsset.duration);
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);
        [ex exportAsynchronouslyWithCompletionHandler:^{
            dispatch_semaphore_signal(sem);
        }];
        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(180 * NSEC_PER_SEC)));
        bool ok = (ex.status == AVAssetExportStatusCompleted);
        if (ok) {
            [fm removeItemAtURL:videoURL error:nil];
            ok = [fm moveItemAtURL:tmpURL toURL:videoURL error:&err];
            if (!ok) {
                NSLog(@"[phira-video] mux: 替换原文件失败 %@", err);
            }
        } else {
            NSLog(@"[phira-video] mux: 导出失败 %@", ex.error);
            [fm removeItemAtURL:tmpURL error:nil];
        }
        [ex release];
        return ok;
    }
}
