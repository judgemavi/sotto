#ifndef SOTTO_CAPTURE_BRIDGE_H
#define SOTTO_CAPTURE_BRIDGE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef void (*sotto_audio_callback)(void *context, const float *samples,
                                     size_t count, uint64_t sequence,
                                     uint64_t stream_time_ns);

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

typedef void (*sotto_error_callback)(void *context, int32_t code);

/* Status/error codes: 1 running, 2 stopped, -2 start failed, -3 revoked,
 * -4 denied, -5 selected target disappeared, -6 user stopped sharing. */

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
} sotto_capture_config;

void sotto_capture_pick_target(sotto_target_callback on_choice, void *context);
void sotto_capture_release_target(void *target);
void *sotto_capture_start_with_target(const sotto_capture_config *config,
                                      void *target);
void sotto_capture_stop(void *handle);
int32_t sotto_capture_permission_status(void);
bool sotto_capture_request_permission(void);
bool sotto_capture_open_permission_settings(void);

#endif
