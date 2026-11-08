// Opt-in hardware benchmark. Production RTX HDR and NVENC native code is linked
// directly; only source generation, scheduling and the historical control
// differ.
#include "internal.hpp"
extern "C" {
#include "adapter.h"
#define _Static_assert static_assert
#include "gpu_snapshot.h"
#undef _Static_assert
void *am_rtx_test_source_format(void *, AmaterasuDmaBufFrame *, uint32_t);
void am_rtx_test_source_close(void *, void *);
int am_rtx_test_read_cuda(void *, uint64_t, uint32_t, uint32_t *, size_t);
}
#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <deque>
#include <iostream>
#include <mutex>
#include <thread>
#include <unistd.h>
#include <vector>
using Clock = std::chrono::steady_clock;
static double us(Clock::duration d) {
  return std::chrono::duration<double, std::micro>(d).count();
}
static void check(int code) {
  if (code)
    throw std::runtime_error("native operation failed: " +
                             std::to_string(code));
}

// Frozen pre-fix control: keep the input mapped through inference/output.
// This deliberately reproduces 6d537e4; the candidate calls run_cuda itself.
static int historical(rtx::Session &s, const AmaterasuDmaBufFrame &frame,
                      rtx::CudaOutput &out) {
  s.surface.wait_acquire(frame.planes[0].acquire_fence_fd);
  s.surface.capture(frame);
  int fence = s.surface.complete();
  rtx::cuda_check(s.api->map(1, &s.input_resource, s.stream), "control map");
  CUarray array = nullptr;
  rtx::cuda_check(s.api->mapped_array(&array, s.input_resource, 0, 0),
                  "control array");
  CUDA_MEMCPY2D copy{};
  copy.srcMemoryType = CU_MEMORYTYPE_ARRAY;
  copy.srcArray = array;
  copy.dstMemoryType = CU_MEMORYTYPE_DEVICE;
  copy.dstDevice = reinterpret_cast<CUdeviceptr>(s.input.pixels);
  copy.dstPitch = s.input.pitch;
  copy.WidthInBytes = s.input.width * 4;
  copy.Height = s.input.height;
  rtx::cuda_check(s.api->copy(&copy, s.stream), "control copy");
  if (s.enabled)
    rtx::vfx_check(s.api->run_effect(s.effect, 1), "control TrueHDR");
  s.write_output(out);
  rtx::cuda_check(s.api->record_event(out.ready, s.stream), "control ready");
  rtx::cuda_check(s.api->unmap(1, &s.input_resource, s.stream),
                  "control unmap");
  return fence;
}
struct StageEvents {
  CUevent model_begin{}, model_end{}, output_begin{}, output_end{};
};
static thread_local StageEvents *active_events;
static rtx::Api *timed_api;
static CUstream timed_stream;
static decltype(rtx::Api{}.run_effect) vendor_run;
static decltype(rtx::Api{}.launch) driver_launch;
static int timed_run(void *effect, int async) {
  rtx::cuda_check(
      timed_api->record_event(active_events->model_begin, timed_stream),
      "model begin");
  int result = vendor_run(effect, async);
  rtx::cuda_check(
      timed_api->record_event(active_events->model_end, timed_stream),
      "model end");
  return result;
}
static CUresult CUDAAPI timed_launch(CUfunction function, unsigned x,
                                     unsigned y, unsigned z, unsigned bx,
                                     unsigned by, unsigned bz, unsigned shared,
                                     CUstream stream, void **args,
                                     void **extra) {
  rtx::cuda_check(timed_api->record_event(active_events->output_begin, stream),
                  "output begin");
  auto result =
      driver_launch(function, x, y, z, bx, by, bz, shared, stream, args, extra);
  rtx::cuda_check(timed_api->record_event(active_events->output_end, stream),
                  "output end");
  return result;
}
struct Slot {
  int snapshot_fd = -1, snapshot_fence = -1;
  StageEvents events;
  void *output{};
  uint64_t pointer{};
  uint32_t pitch{};
  void *ready{};
  Clock::time_point start;
  double dispatch{};
};
static void summary(const char *name, std::vector<double> values) {
  std::sort(values.begin(), values.end());
  double sum = 0;
  for (double v : values)
    sum += v;
  std::cout << '"' << name << "\":{\"mean_us\":" << sum / values.size()
            << ",\"p50_us\":" << values[values.size() / 2]
            << ",\"p95_us\":" << values[(values.size() - 1) * 95 / 100]
            << ",\"max_us\":" << values.back() << '}';
}
int main(int argc, char **argv) {
  if (argc != 7 && argc != 8) {
    std::cerr << "usage: bench RUNTIME current|historical|bypass WIDTH HEIGHT "
                 "FRAMES FPS_OR_ZERO [INJECT_COMPLETION_DELAY_US]\n";
    return 2;
  }
  try {
    const bool snapshot_enabled =
        std::getenv("AMATERASU_BENCH_SNAPSHOT") != nullptr;
    const unsigned delay = argc == 8 ? std::stoul(argv[7]) : 0;
    const std::string mode = argv[2];
    if (mode != "current" && mode != "historical" && mode != "bypass")
      throw std::runtime_error("invalid mode");
    unsigned width = std::stoul(argv[3]), height = std::stoul(argv[4]),
             frames = std::stoul(argv[5]), pace = std::stoul(argv[6]);
    if (!width || !height || width % 2 || height % 2 || !frames ||
        frames > 100000 || pace > 1000)
      throw std::runtime_error("invalid dimensions/count/pace");
    uint32_t parameters[] = {100, 100, 50, 1000, 0};
    if (const char *peak = std::getenv("AMATERASU_BENCH_PEAK"))
      parameters[3] = std::stoul(peak);
    char error[512]{};
    auto *raw = am_rtx_open(argv[1], 0, width, height, parameters, error,
                            sizeof(error));
    if (!raw)
      throw std::runtime_error(error);
    auto &session = *static_cast<rtx::Session *>(raw);
    session.enabled = mode != "bypass";
    AmaterasuDmaBufFrame source_frame{};
    void *source = am_rtx_test_source_format(
        raw, &source_frame,
        uint32_t('A') | (uint32_t('B') << 8) | (uint32_t('2') << 16) |
            (uint32_t('4') << 24));
    if (!source)
      throw std::runtime_error("source allocation failed");
    Slot slots[2];
    timed_api = session.api.get();
    timed_stream = session.stream;
    vendor_run = timed_api->run_effect;
    driver_launch = timed_api->launch;
    timed_api->run_effect = timed_run;
    timed_api->launch = timed_launch;
    {
      rtx::Current current(*session.api, session.surface, session.context);
      for (auto &slot : slots)
        for (auto *event : {&slot.events.model_begin, &slot.events.model_end,
                            &slot.events.output_begin, &slot.events.output_end})
          rtx::cuda_check(session.api->create_event(event, CU_EVENT_DEFAULT),
                          "stage event");
    }
    for (auto &s : slots) {
      s.output = am_rtx_cuda_output_open(raw, &s.pointer, &s.pitch, &s.ready,
                                         error, sizeof(error));
      if (!s.output)
        throw std::runtime_error(error);
    }
    am_nvcuda_struct_versions versions{};
    check(amaterasu_nvcuda_header_versions(&versions));
    am_nvcuda_status status{};
    am_nvcuda_device devices[16]{};
    uint32_t count = 0;
    check(
        amaterasu_nvcuda_enumerate(&versions, 1, devices, 16, &count, &status));
    if (!count)
      throw std::runtime_error("no NVENC device");
    am_nvcuda_settings settings{};
    settings.codec = AM_NVCUDA_CODEC_HEVC;
    settings.bit_depth = 10;
    settings.width = width;
    settings.height = height;
    settings.fps = pace ? pace : 120;
    settings.bitrate = 40000000;
    settings.reference_frames = 1;
    settings.color_primaries = 9;
    settings.transfer_characteristics = 16;
    settings.matrix_coefficients = 9;
    settings.chroma_sample_location = 1;
    const double kr = .2627, kb = .0593, kg = 1 - kr - kb, ys = 876. / 1023,
                 cs = 896. / 1023;
    double y[] = {kr, kg, kb},
           cb[] = {-kr / (2 * (1 - kb)), -kg / (2 * (1 - kb)), .5},
           cr[] = {.5, -kg / (2 * (1 - kr)), -kb / (2 * (1 - kr))};
    for (int i = 0; i < 3; i++) {
      settings.luma_row[i] = y[i] * ys;
      settings.cb_row[i] = cb[i] * cs;
      settings.cr_row[i] = cr[i] * cs;
    }
    settings.luma_row[3] = 64. / 1023;
    settings.cb_row[3] = settings.cr_row[3] = 512. / 1023;
    int result = 0;
    auto *encoder = amaterasu_nvcuda_open(devices[0].pci_bus_id, &versions, 1,
                                          &settings, &status, &result);
    check(result);
    if (!encoder)
      throw std::runtime_error("encoder open failed");
    uint8_t uuid[16]{}, producer_uuid[16]{};
    if (!amaterasu_nvcuda_cuda_input(encoder, uuid))
      throw std::runtime_error("CUDA input unavailable");
    check(am_rtx_device_uuid(argv[1], 0, producer_uuid, error, sizeof(error)));
    if (!std::equal(uuid, uuid + 16, producer_uuid))
      throw std::runtime_error("producer/encoder GPU mismatch");
    std::mutex mutex;
    std::condition_variable cv;
    std::deque<unsigned> pending;
    bool free[2] = {true, true}, finished = false;
    std::vector<double> total, dispatch, model_gpu, output_gpu;
    const unsigned warmup = 30;
    size_t bytes = 0;
    std::thread output([&] {
      if (amaterasu_nvcuda_bind_thread(encoder) != 0)
        std::abort();
      for (unsigned n = 0; n < frames + warmup; n++) {
        unsigned slot;
        {
          std::unique_lock lock(mutex);
          cv.wait(lock, [&] { return !pending.empty(); });
          slot = pending.front();
          pending.pop_front();
        }
        const uint8_t *packet = nullptr;
        size_t length = 0;
        uint32_t type = 0;
        am_nvcuda_status st{};
        if (delay)
          std::this_thread::sleep_for(std::chrono::microseconds(delay));
        if (amaterasu_nvcuda_lock(encoder, slot, &packet, &length, &type,
                                  &st) ||
            !length)
          std::abort();
        auto done = Clock::now();
        float model_ms = 0, output_ms = 0;
        auto &events = slots[slot].events;
        if (mode != "bypass")
          rtx::cuda_check(session.api->elapsed_time(
                              &model_ms, events.model_begin, events.model_end),
                          "model elapsed");
        rtx::cuda_check(session.api->elapsed_time(
                            &output_ms, events.output_begin, events.output_end),
                        "output elapsed");
        if (n >= warmup) {
          total.push_back(us(done - slots[slot].start));
          dispatch.push_back(slots[slot].dispatch);
          bytes += length;
          model_gpu.push_back(model_ms * 1000);
          output_gpu.push_back(output_ms * 1000);
        }
        if (amaterasu_nvcuda_release_output(encoder, slot, &st))
          std::abort();
        {
          std::lock_guard lock(mutex);
          free[slot] = true;
        }
        cv.notify_all();
      }
      amaterasu_nvcuda_unbind_thread(encoder);
      {
        std::lock_guard lock(mutex);
        finished = true;
      }
      cv.notify_all();
    });
    am_vkms_snapshotter *snapshotter =
        snapshot_enabled ? am_vkms_snapshot_create() : nullptr;
    if (snapshot_enabled && !snapshotter)
      std::abort();
    auto schedule = Clock::now();
    for (unsigned n = 0; n < frames + warmup; n++) {
      if (pace)
        std::this_thread::sleep_until(
            schedule +
            std::chrono::nanoseconds(uint64_t(n) * 1000000000 / pace));
      unsigned slot = n % 2;
      {
        std::unique_lock lock(mutex);
        cv.wait(lock, [&] { return free[slot]; });
        free[slot] = false;
      }
      auto &s = slots[slot];
      active_events = &s.events;
      s.start = Clock::now();
      AmaterasuDmaBufFrame processing_frame = source_frame;
      if (snapshot_enabled) {
        if (am_vkms_snapshot_poll(snapshotter, 1000000000) != 1)
          std::abort();
        if (s.snapshot_fd >= 0)
          ::close(s.snapshot_fd);
        if (s.snapshot_fence >= 0)
          ::close(s.snapshot_fence);
        am_vkms_snapshot_input input{};
        input.width = width;
        input.height = height;
        input.fourcc = source_frame.fourcc;
        input.plane_count = 1;
        input.modifier = source_frame.modifier;
        for (unsigned i = 0; i < 4; i++) {
          input.fds[i] = -1;
          input.acquire_fences[i] = -1;
        }
        input.fds[0] = source_frame.planes[0].fd;
        input.pitches[0] = source_frame.planes[0].pitch;
        input.offsets[0] = source_frame.planes[0].offset;
        am_vkms_snapshot_output captured{};
        if (am_vkms_snapshot_copy(snapshotter, &input, &captured))
          std::abort();
        s.snapshot_fd = captured.fd;
        s.snapshot_fence = captured.completion_fd;
        processing_frame.fourcc = captured.fourcc;
        processing_frame.modifier = captured.modifier;
        processing_frame.planes[0] = {captured.fd, captured.completion_fd, 0,
                                      captured.pitch};
      }
      {
        rtx::Current current(*session.api, session.surface, session.context);
        int fence =
            mode == "historical"
                ? historical(session, processing_frame,
                             *static_cast<rtx::CudaOutput *>(s.output))
                : session.run_cuda(processing_frame,
                                   *static_cast<rtx::CudaOutput *>(s.output));
        if (fence >= 0)
          ::close(fence);
      }
      if (n ==
          0) { // Validate real pixels during warm-up, outside measured samples.
        std::vector<uint32_t> pixels(size_t(width) * height);
        check(am_rtx_test_read_cuda(raw, s.pointer, s.pitch, pixels.data(),
                                    pixels.size()));
        if ((pixels[0] & 0x3fffffff) == (pixels[width - 1] & 0x3fffffff))
          throw std::runtime_error("source/output color bars collapsed");
      }
      check(amaterasu_nvcuda_import_cuda(encoder, slot, s.pointer, s.pitch,
                                         width, height, s.ready, &status));
      check(amaterasu_nvcuda_submit(encoder, slot,
                                    int64_t(n) * 10000000 / settings.fps,
                                    n == 0, &status));
      s.dispatch = us(Clock::now() - s.start);
      {
        std::lock_guard lock(mutex);
        pending.push_back(slot);
      }
      cv.notify_all();
    }
    {
      std::unique_lock lock(mutex);
      cv.wait(lock, [&] { return finished; });
    }
    output.join();
    std::cout << "{\"mode\":\"" << mode << "\",\"width\":" << width
              << ",\"height\":" << height << ",\"peak_nits\":" << parameters[3]
              << ",\"frames\":" << frames << ",\"pace_fps\":" << pace
              << ",\"injected_completion_delay_us\":" << delay
              << ",\"snapshot\":" << (snapshot_enabled ? "true" : "false")
              << ",\"debanding\":true,\"encoded_bytes\":" << bytes << ',';
    summary("source_to_bitstream", total);
    std::cout << ',';
    summary("dispatch", dispatch);
    std::cout << ',';
    summary("model_gpu", model_gpu);
    std::cout << ',';
    summary("output_gpu", output_gpu);
    std::cout << "}\n";
    check(amaterasu_nvcuda_drain(encoder, &status));
    amaterasu_nvcuda_close(encoder);
    timed_api->run_effect = vendor_run;
    timed_api->launch = driver_launch;
    {
      rtx::Current current(*session.api, session.surface, session.context);
      for (auto &slot : slots)
        for (auto event : {slot.events.model_begin, slot.events.model_end,
                           slot.events.output_begin, slot.events.output_end})
          rtx::cuda_check(session.api->destroy_event(event),
                          "destroy stage event");
    }
    for (auto &s : slots)
      am_rtx_cuda_output_close(raw, s.output);
    if (snapshotter)
      am_vkms_snapshot_destroy(snapshotter);
    for (auto &s : slots) {
      if (s.snapshot_fd >= 0)
        ::close(s.snapshot_fd);
      if (s.snapshot_fence >= 0)
        ::close(s.snapshot_fence);
    }
    am_rtx_test_source_close(raw, source);
    am_rtx_close(raw);
  } catch (const std::exception &e) {
    std::cerr << e.what() << '\n';
    return 1;
  }
}
