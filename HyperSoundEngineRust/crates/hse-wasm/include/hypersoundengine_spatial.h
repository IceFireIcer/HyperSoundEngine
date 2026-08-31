#ifndef HYPERSOUNDENGINE_SPATIAL_H
#define HYPERSOUNDENGINE_SPATIAL_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define HSE_SPATIAL_ABI_VERSION 1u

uint32_t spatial_abi_version(void);
uint32_t spatial_load_hrtf(const uint8_t *data, size_t data_len,
                           uint32_t sample_rate, uint32_t max_objects,
                           uint32_t max_frames);
int32_t spatial_get_hrir(uint32_t handle, float azimuth_deg, float elevation_deg,
                         float *out_l, size_t out_l_len,
                         float *out_r, size_t out_r_len);
int32_t spatial_render_objects(uint32_t handle,
                               const float *input, size_t input_len, size_t input_stride,
                               const uint32_t *object_slots, size_t object_slots_len,
                               const float *object_params, size_t object_params_len,
                               uint32_t object_count,
                               float *out_l, size_t out_l_len,
                               float *out_r, size_t out_r_len,
                               uint32_t frame_count);
int32_t spatial_set_room(uint32_t handle, float width, float height, float depth,
                         float reflectivity, uint32_t early_orders,
                         float rt60, float amount);
int32_t spatial_set_room_preset(uint32_t handle, uint32_t preset, float amount);
int32_t spatial_set_hrtf_interp_mode(uint32_t handle, uint32_t mode);
int32_t spatial_set_convolution_mode(uint32_t handle, uint32_t mode);
int32_t spatial_set_distance_model(uint32_t handle, uint32_t model,
                                   float reference_distance,
                                   float maximum_distance,
                                   float rolloff_factor);
int32_t spatial_destroy(uint32_t handle);
int32_t spatial_reset_slot(uint32_t handle, uint32_t slot);
size_t spatial_hrir_length(uint32_t handle);
int32_t spatial_last_error_code(uint32_t handle);
size_t spatial_last_error_copy(uint32_t handle, uint8_t *out, size_t out_len);

#ifdef __cplusplus
}
#endif

#endif
