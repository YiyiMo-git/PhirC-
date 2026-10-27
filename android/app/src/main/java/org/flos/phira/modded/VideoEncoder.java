package org.flos.phira.modded;

import android.content.Context;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.media.MediaExtractor;
import android.media.MediaMuxer;
import android.util.Log;

import java.io.File;
import java.nio.ByteBuffer;

/**
 * 离线渲染的安卓视频编码器：MediaCodec（硬件 H.264）+ MediaMuxer 封装 mp4。
 *
 * <p>Rust 侧（phira/src/record.rs 的 android_sink）把每帧 RGBA 转成 NV12 后调
 * {@link #writeFrame(byte[], long)}。**对外全是静态方法**，这样 JNI 只需要
 * find_class + call_static_method，不用跨语言持有对象引用。
 *
 * <p>输出落在 {@code getExternalFilesDir(null)}（应用专属外部目录，无需存储权限，
 * 卸载即清理），文件名由 Rust 传入。
 *
 * <p>⚠️ 与项目其它 Java 文件一致：渲染线程直接调用，不用 Looper。
 */
public final class VideoEncoder {
    private static final String TAG = "PhiraVideo";
    private static final long TIMEOUT_US = 10_000L;

    private static VideoEncoder instance;

    private MediaCodec codec;
    private MediaMuxer muxer;
    private final MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();
    private int trackIndex = -1;
    private boolean muxerStarted;
    private int width;
    private int height;
    private long frames;
    private String outputPath;

    // —— 音频（原文件直通封装：不解码、不重编码，保原音质）——
    private MediaExtractor audioExtractor;
    private int audioTrack = -1;
    private int audioTrackIndex = -1;
    private long audioDelayUs;
    private long audioSamples;
    private long lastVideoPtsUs;

    // ---------------- 静态门面（JNI 只调这些） ----------------

    /** 开始编码。返回 false 表示失败（日志里有原因）。 */
    public static boolean start(Context ctx, String fileName, int w, int h, int fps, int bitrateKbps) {
        return start(ctx, fileName, w, h, fps, bitrateKbps, "", 0L);
    }

    /**
     * 开始编码（带音频）。
     *
     * @param musicPath 音乐文件绝对路径；空串 = 不要音轨
     * @param delayUs   音轨整体延迟（微秒），用于对齐视频片头
     */
    public static boolean start(Context ctx, String fileName, int w, int h, int fps, int bitrateKbps,
                                String musicPath, long delayUs) {
        cancel();
        VideoEncoder e = new VideoEncoder();
        if (!e.startInternal(ctx, fileName, w, h, fps, bitrateKbps, musicPath, delayUs)) {
            return false;
        }
        instance = e;
        return true;
    }

    /** 写一帧 NV12（Y 平面 w*h + 交错 UV w*h/2）。 */
    public static boolean writeFrame(byte[] nv12, long ptsUs) {
        VideoEncoder e = instance;
        return e != null && e.writeInternal(nv12, ptsUs);
    }

    /** 结束并封装。 */
    public static boolean finish() {
        VideoEncoder e = instance;
        instance = null;
        return e != null && e.finishInternal();
    }

    /** 中途取消（异常路径，不产出可用文件）。 */
    public static void cancel() {
        VideoEncoder e = instance;
        instance = null;
        if (e != null) {
            e.release();
        }
    }

    /** 输出文件绝对路径（start 成功后有效）。 */
    public static String getOutputPath() {
        VideoEncoder e = instance;
        return e != null ? e.outputPath : "";
    }

    // ---------------- 实例实现 ----------------

    private boolean startInternal(Context ctx, String fileName, int w, int h, int fps, int bitrateKbps,
                                  String musicPath, long delayUs) {
        try {
            File dir = ctx.getExternalFilesDir(null);
            if (dir == null) {
                dir = ctx.getFilesDir();
            }
            File out = new File(dir, fileName);
            // MediaMuxer 要求目标文件不存在
            if (out.exists() && !out.delete()) {
                Log.w(TAG, "无法删除已有文件: " + out);
            }
            width = w;
            height = h;
            outputPath = out.getAbsolutePath();

            MediaFormat format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, w, h);
            format.setInteger(MediaFormat.KEY_BIT_RATE, bitrateKbps * 1000);
            format.setInteger(MediaFormat.KEY_FRAME_RATE, fps);
            format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
            format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                    MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420SemiPlanar);

            codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
            try {
                codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            } catch (Throwable t) {
                // 少数设备不接受 SemiPlanar，退回 Flexible（仍按 NV12 喂）
                Log.w(TAG, "SemiPlanar 不被接受，改用 Flexible: " + t);
                format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
                        MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
                codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            }
            codec.start();

            muxer = new MediaMuxer(outputPath, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4);
            trackIndex = -1;
            muxerStarted = false;
            frames = 0;
            audioDelayUs = Math.max(0L, delayUs);
            // 音轨必须在 muxer.start() 之前 addTrack —— 视频轨要等首帧格式才知道。
            // 失败就当无声视频，不影响渲染本身。
            if (musicPath != null && !musicPath.isEmpty()) {
                try {
                    MediaExtractor ex = new MediaExtractor();
                    ex.setDataSource(musicPath);
                    int track = -1;
                    for (int i = 0; i < ex.getTrackCount(); i++) {
                        MediaFormat f = ex.getTrackFormat(i);
                        String mime = f.getString(MediaFormat.KEY_MIME);
                        if (mime != null && mime.startsWith("audio/")) {
                            track = i;
                            audioTrackIndex = muxer.addTrack(f);
                            break;
                        }
                    }
                    if (track >= 0) {
                        audioTrack = track;
                        audioExtractor = ex;
                        Log.i(TAG, "音轨已加入: " + musicPath + "（延迟 " + audioDelayUs + "us）");
                    } else {
                        ex.release();
                        Log.w(TAG, "音乐里没有音轨，按无声渲染: " + musicPath);
                    }
                } catch (Throwable t) {
                    Log.w(TAG, "打开音轨失败，按无声渲染: " + musicPath, t);
                    audioExtractor = null;
                    audioTrackIndex = -1;
                }
            }
            Log.i(TAG, "开始编码 " + outputPath + " " + w + "x" + h + "@" + fps + " " + bitrateKbps + "kbps");
            return true;
        } catch (Throwable t) {
            Log.e(TAG, "start 失败", t);
            release();
            return false;
        }
    }

    private boolean writeInternal(byte[] nv12, long ptsUs) {
        if (codec == null) {
            return false;
        }
        try {
            int inIndex = codec.dequeueInputBuffer(TIMEOUT_US);
            if (inIndex < 0) {
                Log.w(TAG, "拿不到输入 buffer，丢帧 " + frames);
                return false;
            }
            ByteBuffer in = codec.getInputBuffer(inIndex);
            if (in == null) {
                return false;
            }
            in.clear();
            in.put(nv12);
            codec.queueInputBuffer(inIndex, 0, nv12.length, ptsUs, 0);
            frames++;
            lastVideoPtsUs = ptsUs;
            drain(false);
            return true;
        } catch (Throwable t) {
            Log.e(TAG, "writeFrame 失败", t);
            return false;
        }
    }

    private boolean finishInternal() {
        if (codec == null) {
            return false;
        }
        boolean ok = false;
        try {
            int inIndex = codec.dequeueInputBuffer(TIMEOUT_US * 10);
            if (inIndex >= 0) {
                codec.queueInputBuffer(inIndex, 0, 0, frames * 1000, MediaCodec.BUFFER_FLAG_END_OF_STREAM);
            }
            drain(true);
            writeAudio();
            ok = muxerStarted;
            Log.i(TAG, "编码结束: " + outputPath + "，共 " + frames + " 帧 / 音频 " + audioSamples + " 块");
        } catch (Throwable t) {
            Log.e(TAG, "finish 失败", t);
        } finally {
            release();
        }
        return ok;
    }

    /**
     * 把音乐文件的压缩音频块原样写进 muxer（时间戳加片头延迟）。
     *
     * <p>MediaMuxer 只要求**同一轨**内时间戳递增，跨轨可以先后写 ——
     * 所以视频全写完再一次性写音频是合法的，也最简单。
     */
    private void writeAudio() {
        MediaExtractor ex = audioExtractor;
        if (ex == null || audioTrackIndex < 0 || !muxerStarted) {
            return;
        }
        try {
            ex.selectTrack(audioTrack);
            ByteBuffer buf = ByteBuffer.allocateDirect(1 << 20);
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            long endUs = lastVideoPtsUs + 200_000L; // 视频结束后 0.2s 内截断
            while (true) {
                int size = ex.readSampleData(buf, 0);
                if (size < 0) {
                    break;
                }
                long pts = ex.getSampleTime() + audioDelayUs;
                if (pts > endUs) {
                    break;
                }
                info.set(0, size, pts, ex.getSampleFlags());
                buf.position(0);
                buf.limit(size);
                try {
                    muxer.writeSampleData(audioTrackIndex, buf, info);
                    audioSamples++;
                } catch (Throwable t) {
                    Log.w(TAG, "写音频块失败（pts=" + pts + "），停止", t);
                    break;
                }
                if (!ex.advance()) {
                    break;
                }
            }
        } catch (Throwable t) {
            Log.w(TAG, "写音轨失败（视频照常保留）", t);
        }
    }

    private void drain(boolean endOfStream) {
        while (true) {
            int outIndex = codec.dequeueOutputBuffer(bufferInfo, TIMEOUT_US);
            if (outIndex == MediaCodec.INFO_TRY_AGAIN_LATER) {
                if (!endOfStream) {
                    return;
                }
                continue;
            }
            if (outIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                if (!muxerStarted) {
                    trackIndex = muxer.addTrack(codec.getOutputFormat());
                    muxer.start();
                    muxerStarted = true;
                }
                continue;
            }
            if (outIndex < 0) {
                continue; // INFO_OUTPUT_BUFFERS_CHANGED 等
            }
            ByteBuffer out = codec.getOutputBuffer(outIndex);
            if (out != null && bufferInfo.size > 0 && muxerStarted) {
                out.position(bufferInfo.offset);
                out.limit(bufferInfo.offset + bufferInfo.size);
                muxer.writeSampleData(trackIndex, out, bufferInfo);
            }
            codec.releaseOutputBuffer(outIndex, false);
            if ((bufferInfo.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0) {
                return;
            }
        }
    }

    private void release() {
        if (audioExtractor != null) {
            try {
                audioExtractor.release();
            } catch (Throwable ignored) {
            }
            audioExtractor = null;
        }
        if (codec != null) {
            try {
                codec.stop();
            } catch (Throwable ignored) {
            }
            try {
                codec.release();
            } catch (Throwable ignored) {
            }
            codec = null;
        }
        if (muxer != null) {
            try {
                if (muxerStarted) {
                    muxer.stop();
                }
            } catch (Throwable t) {
                Log.w(TAG, "muxer.stop 失败: " + t);
            }
            try {
                muxer.release();
            } catch (Throwable ignored) {
            }
            muxer = null;
            muxerStarted = false;
        }
    }
}
