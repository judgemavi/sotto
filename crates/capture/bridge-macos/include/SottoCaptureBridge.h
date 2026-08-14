#ifndef SOTTO_CAPTURE_BRIDGE_H
#define SOTTO_CAPTURE_BRIDGE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef void (*sotto_audio_callback)(void *context, const float *samples,
                                     size_t count, uint64_t sequence,
                                     uint64_t stream_time_ns);
/* Audio samples are mono Float32 at exactly 48 kHz. The Swift bridge reports
 * terminal error -7 instead of invoking this callback for any other actual
 * ScreenCaptureKit format, because sample-rate metadata is not part of this ABI. */

/*
 * `bytes` is BGRA8888 and is valid only for the duration of this callback.
 * Receivers must copy it before returning. `stride` is authoritative and may
 * exceed width * 4. stream_time_ns is the SCStream presentation timestamp;
 * host_time_ns is mach continuous time converted to nanoseconds.
 *
 * The bridge invokes video on a queue separate from audio. Rust uses a bounded
 * preallocated pool and drops the newest frame if that pool is exhausted, so a
 * stalled frame consumer cannot delay audio capture.
 */
typedef void (*sotto_frame_callback)(void *context, const uint8_t *bytes,
                                     size_t length, uint32_t width,
                                     uint32_t height, uint32_t stride,
                                     uint32_t pixel_format,
                                     uint64_t stream_time_ns,
                                     uint64_t host_time_ns);

/* `detail` is optional UTF-8 and valid only for the duration of the callback. */
typedef void (*sotto_error_callback)(void *context, int32_t code,
                                     const char *detail);

/* Status/error codes: 1 running, 2 stopped, -2 start failed, -3 revoked,
 * -4 denied, -5 selected target disappeared, -6 user stopped sharing,
 * -7 unsupported or unreadable system-audio sample format,
 * -8 local recording write failure; detail carries the native writer cause. */

typedef enum {
  SOTTO_CAPTURE_TARGET_APPLICATION = 1,
  SOTTO_CAPTURE_TARGET_WINDOW = 2,
  SOTTO_CAPTURE_TARGET_DISPLAY = 3,
} sotto_capture_target_kind;

/* Strings are UTF-8 and valid only for the duration of the callback. A null
 * target is a normal cancellation (all description fields are then null/0). */
typedef void (*sotto_target_callback)(
    void *context, void *target, const char *bundle_id,
    const char *display_name, const char *window_title,
    sotto_capture_target_kind kind, bool audio_scoped);

typedef struct {
  sotto_audio_callback audio;
  sotto_error_callback error;
  sotto_frame_callback frame;
  void *context;
  const char *recording_path;
} sotto_capture_config;

void sotto_capture_pick_target(sotto_target_callback on_choice, void *context);
void sotto_capture_release_target(void *target);
void *sotto_capture_start_with_target(const sotto_capture_config *config,
                                      void *target);
/* Starts microphone recording without presenting a picker or constructing an
 * SCContentFilter/SCStream. Microphone samples arrive through append below. */
void *sotto_capture_start_microphone_only(const sotto_capture_config *config);
void sotto_capture_append_microphone(void *handle, const float *samples,
                                     size_t count, uint32_t sample_rate,
                                     uint32_t channels,
                                     uint64_t stream_time_ns);
void sotto_capture_stop(void *handle);
bool sotto_recording_probe(const char *path, uint64_t *duration_ns,
                           uint64_t *byte_size, uint64_t *first_video_ns,
                           uint64_t *seek_video_ns);
bool sotto_recording_committed_duration(const char *path, uint64_t *duration_ns,
                                        uint64_t *byte_size);
bool sotto_recording_append_pts_probe(const int64_t *input, int64_t *output,
                                      size_t count);
int32_t sotto_capture_permission_status(void);
bool sotto_capture_request_permission(void);
bool sotto_capture_open_permission_settings(void);

/*
 * Runs the main run loop for `seconds`, draining the main queue so the picker
 * task scheduled by sotto_capture_pick_target can actually run.
 *
 * Must be called on the main thread, and only by processes that do not already
 * run an AppKit event loop. A process that blocks its main thread waiting for
 * the picker callback deadlocks: the callback is delivered on the main queue.
 */
void sotto_capture_pump_main_loop(double seconds);

#endif
