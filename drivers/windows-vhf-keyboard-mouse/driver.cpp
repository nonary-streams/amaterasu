// Copyright (c) 2026 Chase Payne
// SPDX-License-Identifier: MIT
// UMDF2 VHF source for one keyboard and one relative mouse. No publisher code,
// policy, or arbitrary HID descriptor is accepted at this boundary.
#define WIN32_NO_STATUS
#include <windows.h>
#undef WIN32_NO_STATUS
#include <wdf.h>
#include <vhf.h>

#include <cstdint>
#include <cstring>
#include "protocol.h"

namespace {

namespace wire = amaterasu::vhf;
constexpr std::uint32_t queue_capacity = 64;
constexpr std::uint8_t keyboard_id = 1;
constexpr std::uint8_t mouse_id = 2;

// Six-key rollover is deliberate. The companion rejects a seventh key rather
// than generating an ambiguous HID ErrorRollOver state.
std::uint8_t report_descriptor[] {
  0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, keyboard_id,
  0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
  0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
  0x75, 0x08, 0x95, 0x01, 0x81, 0x01,
  0x75, 0x08, 0x95, 0x06, 0x15, 0x00, 0x26, 0x73, 0x00,
  0x19, 0x00, 0x29, 0x73, 0x81, 0x00, 0xc0,
  0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00,
  0x85, mouse_id,
  0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01,
  0x75, 0x01, 0x95, 0x05, 0x81, 0x02,
  0x75, 0x03, 0x95, 0x01, 0x81, 0x01,
  0x05, 0x01, 0x09, 0x30, 0x09, 0x31,
  0x16, 0x00, 0x80, 0x26, 0xff, 0x7f,
  0x75, 0x10, 0x95, 0x02, 0x81, 0x06,
  0x09, 0x38, 0x95, 0x01, 0x81, 0x06,
  0x05, 0x0c, 0x0a, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06,
  0xc0, 0xc0
};

struct pending_report {
  std::uint8_t bytes[10];
  ULONG length;
  UCHAR id;
};

struct device_context {
  WDFWAITLOCK lifetime;
  WDFWAITLOCK state;
  WDFIOTARGET vhf_target;
  WDFWORKITEM pump;
  WDFWORKITEM teardown;
  WDFTIMER watchdog;
  HANDLE vhf_file;
  VHFHANDLE vhf;
  WDFFILEOBJECT owner;
  bool target_open;
  bool stopping;
  bool starting;
  bool active;
  bool ready;
  bool timed_out;
  bool faulted;
  ULONGLONG deadline_tick;
  ULONGLONG pending_deadline_tick;
  pending_report pending[queue_capacity];
  std::uint32_t head;
  std::uint32_t count;
};

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(device_context, device_state);

EVT_WDF_DRIVER_DEVICE_ADD add_device;
EVT_WDF_DEVICE_PREPARE_HARDWARE prepare_hardware;
EVT_WDF_DEVICE_RELEASE_HARDWARE release_hardware;
EVT_WDF_DEVICE_FILE_CREATE file_create;
EVT_WDF_FILE_CLOSE file_close;
EVT_WDF_IO_QUEUE_IO_DEVICE_CONTROL device_control;
EVT_WDF_WORKITEM pump_reports;
EVT_WDF_WORKITEM teardown_failed_child;
EVT_WDF_TIMER watchdog_tick;
EVT_VHF_READY_FOR_NEXT_READ_REPORT next_report;

void lock(WDFWAITLOCK value) { WdfWaitLockAcquire(value, nullptr); }
void unlock(WDFWAITLOCK value) { WdfWaitLockRelease(value); }

bool append_report(device_context *const context, const pending_report &report) {
  if (context->count == queue_capacity) {
    return false;
  }
  if (context->count == 0) context->pending_deadline_tick = GetTickCount64() + 2000;
  const auto tail = (context->head + context->count) % queue_capacity;
  context->pending[tail] = report;
  ++context->count;
  return true;
}

pending_report keyboard_report(const wire::keyboard_request &request) {
  pending_report report {};
  report.id = keyboard_id;
  report.length = 9;
  report.bytes[0] = keyboard_id;
  report.bytes[1] = request.modifiers;
  std::memcpy(&report.bytes[3], request.keys, 6);
  return report;
}

pending_report mouse_report(const wire::mouse_request &request) {
  pending_report report {};
  report.id = mouse_id;
  report.length = 10;
  report.bytes[0] = mouse_id;
  report.bytes[1] = request.buttons;
  const std::int16_t axes[] {request.delta_x, request.delta_y, request.wheel, request.pan};
  std::memcpy(&report.bytes[2], axes, sizeof(axes));
  return report;
}

bool valid_keyboard(const wire::keyboard_request &request) {
  if (request.reserved != 0) return false;
  bool saw_zero = false;
  for (std::uint8_t key : request.keys) {
    if (key == 0) {
      saw_zero = true;
      continue;
    }
    if (saw_zero || key < 4 || key > 0x73) return false;
  }
  for (std::size_t i = 0; i < 6; ++i) {
    if (request.keys[i] == 0) continue;
    for (std::size_t j = i + 1; j < 6; ++j) {
      if (request.keys[i] == request.keys[j]) return false;
    }
  }
  return true;
}

bool valid_mouse(const wire::mouse_request &request) {
  return request.reserved0 == 0 && request.reserved1 == 0 &&
         (request.buttons & ~0x1fu) == 0;
}

template<class T>
NTSTATUS read_request(WDFREQUEST request, T **const output) {
  PVOID buffer = nullptr;
  size_t length = 0;
  const NTSTATUS status = WdfRequestRetrieveInputBuffer(request, sizeof(wire::request_header), &buffer, &length);
  if (!NT_SUCCESS(status)) return status;
  if (length != sizeof(T)) return STATUS_INVALID_BUFFER_SIZE;
  auto *const value = static_cast<T *>(buffer);
  if (value->header.size != sizeof(T) || value->header.version != wire::protocol_version ||
      value->header.reserved != 0) return STATUS_INVALID_PARAMETER;
  *output = value;
  return STATUS_SUCCESS;
}

NTSTATUS open_vhf_target(device_context *const context) {
  if (context->target_open) return STATUS_SUCCESS;
  lock(context->state);
  const bool stopping = context->stopping;
  unlock(context->state);
  if (stopping) return STATUS_DEVICE_NOT_READY;
  WDF_IO_TARGET_OPEN_PARAMS params;
  WDF_IO_TARGET_OPEN_PARAMS_INIT_OPEN_BY_FILE(&params, nullptr);
  const NTSTATUS status = WdfIoTargetOpen(context->vhf_target, &params);
  if (!NT_SUCCESS(status)) return status;
  context->vhf_file = WdfIoTargetWdmGetTargetFileHandle(context->vhf_target);
  if (context->vhf_file == nullptr) {
    WdfIoTargetClose(context->vhf_target);
    return STATUS_DEVICE_NOT_READY;
  }
  context->target_open = true;
  return STATUS_SUCCESS;
}

void next_report(PVOID client_context) {
  auto *const context = static_cast<device_context *>(client_context);
  lock(context->state);
  context->ready = true;
  const bool active = context->active && !context->stopping;
  unlock(context->state);
  // A callback must not acquire lifetime: VhfDelete waits for callbacks.
  if (active) WdfWorkItemEnqueue(context->pump);
}

// The caller holds lifetime, which serializes child deletion with creation,
// report submission and a delayed fault-teardown work item.
void destroy_child_locked(device_context *const context, WDFFILEOBJECT owner) {
  lock(context->state);
  if (owner != nullptr && context->owner != owner) {
    unlock(context->state);
    return;
  }
  VHFHANDLE child = context->vhf;
  context->vhf = nullptr;
  context->owner = nullptr;
  context->active = false;
  context->faulted = false;
  context->starting = false;
  context->ready = false;
  context->head = 0;
  context->count = 0;
  context->pending_deadline_tick = 0;
  unlock(context->state);
  WdfTimerStop(context->watchdog, TRUE);
  if (child != nullptr) VhfDelete(child, TRUE);
}

void destroy_child(device_context *const context, WDFFILEOBJECT owner) {
  lock(context->lifetime);
  destroy_child_locked(context, owner);
  unlock(context->lifetime);
}

void pump_reports(WDFWORKITEM work_item) {
  auto *const context = device_state(static_cast<WDFDEVICE>(WdfWorkItemGetParentObject(work_item)));
  lock(context->lifetime);
  for (;;) {
    pending_report report {};
    lock(context->state);
    if (!context->active || context->stopping || !context->ready || context->count == 0) {
      unlock(context->state);
      break;
    }
    report = context->pending[context->head];
    context->head = (context->head + 1) % queue_capacity;
    --context->count;
    context->pending_deadline_tick = context->count == 0 ? 0 : GetTickCount64() + 2000;
    context->ready = false;
    const VHFHANDLE handle = context->vhf;
    unlock(context->state);

    HID_XFER_PACKET transfer {report.bytes, report.length, report.id};
    const NTSTATUS status = VhfReadReportSubmit(handle, &transfer);
    if (!NT_SUCCESS(status)) {
      // An uncertain delivery can leave a held key. Stop input immediately
      // and remove the HID child from another work item after lifetime unlock.
      lock(context->state);
      context->active = false;
      context->faulted = true;
      context->count = 0;
      context->pending_deadline_tick = 0;
      unlock(context->state);
      WdfWorkItemEnqueue(context->teardown);
      break;
    }
  }
  unlock(context->lifetime);
}

void teardown_failed_child(WDFWORKITEM work_item) {
  auto *const context = device_state(static_cast<WDFDEVICE>(WdfWorkItemGetParentObject(work_item)));
  lock(context->lifetime);
  lock(context->state);
  const bool faulted = context->faulted;
  unlock(context->state);
  if (faulted) destroy_child_locked(context, nullptr);
  unlock(context->lifetime);
}

NTSTATUS create_child(device_context *const context, WDFFILEOBJECT owner) {
  lock(context->lifetime);
  lock(context->state);
  if (context->owner != nullptr || context->stopping) {
    unlock(context->state);
    unlock(context->lifetime);
    return STATUS_DEVICE_BUSY;
  }
  context->owner = owner;
  context->starting = true;
  context->ready = true;
  unlock(context->state);

  NTSTATUS status = open_vhf_target(context);
  VHFHANDLE child = nullptr;
  if (NT_SUCCESS(status)) {
    VHF_CONFIG config;
    VHF_CONFIG_INIT(&config, context->vhf_file, static_cast<USHORT>(sizeof(report_descriptor)), report_descriptor);
    config.VhfClientContext = context;
    config.EvtVhfReadyForNextReadReport = next_report;
    config.VendorID = 0;  // No third-party VID is claimed before allocation.
    config.ProductID = 0x0000;
    config.VersionNumber = 1;
    status = VhfCreate(&config, &child);
  }
  if (NT_SUCCESS(status)) status = VhfStart(child);
  if (NT_SUCCESS(status)) {
    lock(context->state);
    context->vhf = child;
    context->active = true;
    context->starting = false;
    context->timed_out = false;
    context->faulted = false;
    context->deadline_tick = GetTickCount64() + 2000;
    unlock(context->state);
    WdfTimerStart(context->watchdog, WDF_REL_TIMEOUT_IN_MS(100));
  } else {
    lock(context->state);
    context->starting = false;
    context->owner = nullptr;
    unlock(context->state);
    if (child != nullptr) VhfDelete(child, TRUE);
  }
  unlock(context->lifetime);
  return status;
}

NTSTATUS enqueue(device_context *const context, WDFFILEOBJECT owner, const pending_report &report) {
  lock(context->state);
  if (context->owner != owner || !context->active || context->stopping || context->timed_out) {
    unlock(context->state);
    return STATUS_ACCESS_DENIED;
  }
  const bool accepted = append_report(context, report);
  if (accepted) context->deadline_tick = GetTickCount64() + 2000;
  unlock(context->state);
  if (!accepted) return STATUS_DEVICE_BUSY;
  WdfWorkItemEnqueue(context->pump);
  return STATUS_SUCCESS;
}

NTSTATUS keep_alive(device_context *const context, WDFFILEOBJECT owner) {
  lock(context->state);
  const bool allowed = context->active && !context->stopping && !context->timed_out &&
                       context->owner == owner;
  if (allowed) context->deadline_tick = GetTickCount64() + 2000;
  unlock(context->state);
  return allowed ? STATUS_SUCCESS : STATUS_ACCESS_DENIED;
}

void watchdog_tick(WDFTIMER timer) {
  auto *const context = device_state(static_cast<WDFDEVICE>(WdfTimerGetParentObject(timer)));
  lock(context->state);
  const ULONGLONG now = GetTickCount64();
  const bool expired = context->active && !context->stopping && !context->timed_out &&
                       (now >= context->deadline_tick ||
                        (context->count != 0 && now >= context->pending_deadline_tick));
  const bool faulted = context->faulted;
  if (expired) {
    // Removing the child releases its complete HID state even when VHF has
    // stopped requesting reports. Queued neutral reports cannot ensure this.
    context->timed_out = true;
    context->active = false;
    context->faulted = true;
    context->head = 0;
    context->count = 0;
    context->pending_deadline_tick = 0;
  }
  unlock(context->state);
  if (expired || faulted) WdfWorkItemEnqueue(context->teardown);
}

void device_control(WDFQUEUE queue, WDFREQUEST request, size_t, size_t, ULONG code) {
  auto *const context = device_state(WdfIoQueueGetDevice(queue));
  const WDFFILEOBJECT owner = WdfRequestGetFileObject(request);
  NTSTATUS status = STATUS_INVALID_DEVICE_REQUEST;
  ULONG_PTR written = 0;
  if (owner == nullptr) {
    WdfRequestComplete(request, STATUS_INVALID_HANDLE);
    return;
  }

  switch (code) {
    case wire::query: {
      wire::basic_request *input = nullptr;
      status = read_request(request, &input);
      if (!NT_SUCCESS(status)) break;
      wire::query_response *output = nullptr;
      status = WdfRequestRetrieveOutputBuffer(request, sizeof(*output), reinterpret_cast<PVOID *>(&output), nullptr);
      if (NT_SUCCESS(status)) {
        *output = {{sizeof(*output), wire::protocol_version, 0}, 0x03, queue_capacity};
        written = sizeof(*output);
      }
      break;
    }
    case wire::create: {
      wire::basic_request *input = nullptr;
      status = read_request(request, &input);
      if (NT_SUCCESS(status)) status = create_child(context, owner);
      break;
    }
    case wire::keyboard: {
      wire::keyboard_request *input = nullptr;
      status = read_request(request, &input);
      if (NT_SUCCESS(status)) {
        status = valid_keyboard(*input) ? enqueue(context, owner, keyboard_report(*input)) : STATUS_INVALID_PARAMETER;
      }
      break;
    }
    case wire::mouse: {
      wire::mouse_request *input = nullptr;
      status = read_request(request, &input);
      if (NT_SUCCESS(status)) {
        status = valid_mouse(*input) ? enqueue(context, owner, mouse_report(*input)) : STATUS_INVALID_PARAMETER;
      }
      break;
    }
    case wire::destroy: {
      wire::basic_request *input = nullptr;
      status = read_request(request, &input);
      if (NT_SUCCESS(status)) {
        lock(context->state);
        // Teardown after a fault or missed heartbeat may have already
        // removed this handle's child. Closing an absent child is idempotent;
        // an old handle must never remove a newer owner's child.
        const bool owned_or_absent = context->owner == owner || context->owner == nullptr;
        unlock(context->state);
        status = owned_or_absent ? STATUS_SUCCESS : STATUS_ACCESS_DENIED;
        if (owned_or_absent) destroy_child(context, owner);
      }
      break;
    }
    case wire::heartbeat: {
      wire::basic_request *input = nullptr;
      status = read_request(request, &input);
      if (NT_SUCCESS(status)) status = keep_alive(context, owner);
      break;
    }
    default: break;
  }
  WdfRequestCompleteWithInformation(request, status, written);
}

void file_create(WDFDEVICE, WDFREQUEST request, WDFFILEOBJECT) {
  WdfRequestComplete(request, STATUS_SUCCESS);
}

void file_close(WDFFILEOBJECT object) {
  destroy_child(device_state(WdfFileObjectGetDevice(object)), object);
}

NTSTATUS prepare_hardware(WDFDEVICE device, WDFCMRESLIST, WDFCMRESLIST) {
  auto *const context = device_state(device);
  lock(context->lifetime);
  context->stopping = false;
  // The local target cannot open before PnP start. Retry at create if this
  // first attempt runs before the target is available.
  static_cast<void>(open_vhf_target(context));
  unlock(context->lifetime);
  return STATUS_SUCCESS;
}

NTSTATUS release_hardware(WDFDEVICE device, WDFCMRESLIST) {
  auto *const context = device_state(device);
  lock(context->state);
  context->stopping = true;
  unlock(context->state);
  destroy_child(context, nullptr);
  lock(context->lifetime);
  if (context->target_open) WdfIoTargetClose(context->vhf_target);
  context->target_open = false;
  context->vhf_file = nullptr;
  unlock(context->lifetime);
  return STATUS_SUCCESS;
}

NTSTATUS add_device(WDFDRIVER, PWDFDEVICE_INIT init) {
  WdfFdoInitSetFilter(init);
  WDF_FILEOBJECT_CONFIG file_config;
  WDF_FILEOBJECT_CONFIG_INIT(&file_config, file_create, file_close, WDF_NO_EVENT_CALLBACK);
  WdfDeviceInitSetFileObjectConfig(init, &file_config, WDF_NO_OBJECT_ATTRIBUTES);
  WDF_PNPPOWER_EVENT_CALLBACKS pnp;
  WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnp);
  pnp.EvtDevicePrepareHardware = prepare_hardware;
  pnp.EvtDeviceReleaseHardware = release_hardware;
  WdfDeviceInitSetPnpPowerEventCallbacks(init, &pnp);

  WDF_OBJECT_ATTRIBUTES attributes;
  WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, device_context);
  WDFDEVICE device = nullptr;
  NTSTATUS status = WdfDeviceCreate(&init, &attributes, &device);
  if (!NT_SUCCESS(status)) return status;
  auto *const context = device_state(device);
  std::memset(context, 0, sizeof(*context));

  WDF_OBJECT_ATTRIBUTES child_attributes;
  WDF_OBJECT_ATTRIBUTES_INIT(&child_attributes);
  child_attributes.ParentObject = device;
  status = WdfWaitLockCreate(&child_attributes, &context->lifetime);
  if (!NT_SUCCESS(status)) return status;
  child_attributes.ParentObject = context->lifetime;
  status = WdfWaitLockCreate(&child_attributes, &context->state);
  if (!NT_SUCCESS(status)) return status;
  child_attributes.ParentObject = device;
  status = WdfIoTargetCreate(device, &child_attributes, &context->vhf_target);
  if (!NT_SUCCESS(status)) return status;

  WDF_WORKITEM_CONFIG work_config;
  WDF_WORKITEM_CONFIG_INIT(&work_config, pump_reports);
  status = WdfWorkItemCreate(&work_config, &child_attributes, &context->pump);
  if (!NT_SUCCESS(status)) return status;
  WDF_WORKITEM_CONFIG_INIT(&work_config, teardown_failed_child);
  status = WdfWorkItemCreate(&work_config, &child_attributes, &context->teardown);
  if (!NT_SUCCESS(status)) return status;
  WDF_TIMER_CONFIG timer_config;
  WDF_TIMER_CONFIG_INIT_PERIODIC(&timer_config, watchdog_tick, 100);
  timer_config.AutomaticSerialization = FALSE;
  status = WdfTimerCreate(&timer_config, &child_attributes, &context->watchdog);
  if (!NT_SUCCESS(status)) return status;
  status = WdfDeviceCreateDeviceInterface(device, &wire::interface_guid, nullptr);
  if (!NT_SUCCESS(status)) return status;

  WDF_IO_QUEUE_CONFIG queue;
  WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&queue, WdfIoQueueDispatchSequential);
  queue.PowerManaged = WdfTrue;
  queue.EvtIoDeviceControl = device_control;
  return WdfIoQueueCreate(device, &queue, WDF_NO_OBJECT_ATTRIBUTES, nullptr);
}

}  // namespace

extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT object, PUNICODE_STRING registry_path) {
  WDF_DRIVER_CONFIG config;
  WDF_DRIVER_CONFIG_INIT(&config, add_device);
  return WdfDriverCreate(object, registry_path, WDF_NO_OBJECT_ATTRIBUTES, &config, WDF_NO_HANDLE);
}
